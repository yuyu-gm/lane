use crate::{core::Store, util::*};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

pub fn relative(value: &str) -> Result<PathBuf> {
    let q = PathBuf::from(value.replace('\\', "/"));
    if q.as_os_str().is_empty()
        || q.components().any(|c| !matches!(c, Component::Normal(_)))
        || value.contains(':')
        || q.starts_with(".git")
    {
        return Err(Error::lane(format!("unsafe artifact path: {value}")));
    }
    Ok(q)
}
pub fn validate_contract(c: &Value) -> Result<()> {
    if !c.is_object() {
        return Err(Error::lane("contract must be an object"));
    }
    for k in ["owner", "campaign"] {
        if c.get(k).is_some_and(|v| !v.is_string()) {
            return Err(Error::lane(format!("contract {k} must be a string")));
        }
    }
    for k in [
        "symbols",
        "dependencies",
        "required_checks",
        "required_artifacts",
    ] {
        if let Some(v) = c.get(k)
            && (!v.is_array()
                || v.as_array()
                    .unwrap()
                    .iter()
                    .any(|v| !v.is_string() || v.as_str().unwrap().is_empty()))
        {
            return Err(Error::lane(format!(
                "contract {k} must be an array of nonempty strings"
            )));
        }
    }
    for q in strings(&c["required_artifacts"]) {
        relative(&q)?;
    }
    for dep in strings(&c["dependencies"]) {
        if !oid(&dep) {
            return Err(Error::lane("contract dependencies must be full commit IDs"));
        }
    }
    if c.get("frozen").is_some_and(|v| !v.is_boolean()) {
        return Err(Error::lane("contract frozen must be boolean"));
    }
    if c["frozen"] == true && !oid(&s(c, "frozen_commit")) {
        return Err(Error::lane("frozen contract requires frozen_commit"));
    }
    Ok(())
}
pub fn contract_errors(
    store: &Store,
    m: &Value,
    tip: &str,
    errors: &mut Vec<String>,
) -> Result<()> {
    let Some(c) = m.get("contract") else {
        return Ok(());
    };
    if let Err(e) = validate_contract(c) {
        errors.push(e.message);
        return Ok(());
    }
    if c["frozen"] == true && s(c, "frozen_commit") != tip {
        errors.push("lane tip differs from frozen_commit".into());
    }
    if tip.is_empty() {
        return Ok(());
    }
    for dep in strings(&c["dependencies"]) {
        if !ancestor(&store.repo, &dep, tip)? {
            errors.push(format!(
                "dependency commit is not an ancestor of lane tip: {dep}"
            ));
        }
    }
    for check in strings(&c["required_checks"]) {
        if m["verification"][&check]["commit"] != tip || m["verification"][&check]["passed"] != true
        {
            errors.push(format!(
                "required check has no passing record for lane tip: {check}"
            ));
        }
    }
    Ok(())
}

impl Store {
    pub fn set_contract(
        &self,
        id: &str,
        new: Option<Value>,
        freeze: Option<bool>,
    ) -> Result<Value> {
        self.assert_location(id)?;
        let _lock = self.lock()?;
        let mut m = self.load(id)?;
        if truth(&m["cleaned_at"]) {
            return Err(Error::lane("cannot change a cleaned lane contract"));
        }
        let mut c = new.unwrap_or_else(|| m.get("contract").cloned().unwrap_or(json!({})));
        if let Some(f) = freeze {
            c["frozen"] = json!(f);
            if f {
                c["frozen_commit"] = json!(resolve(
                    &self.repo,
                    &format!("refs/heads/{}", s(&m, "branch"))
                )?);
            } else {
                c.as_object_mut()
                    .ok_or_else(|| Error::lane("contract must be an object"))?
                    .remove("frozen_commit");
            }
        }
        validate_contract(&c)?;
        m["contract"] = c.clone();
        self.save(&m)?;
        Ok(json!({"id":id,"contract":c}))
    }
    pub fn attest(&self, id: &str, check: &str, passed: bool, evidence: &str) -> Result<Value> {
        self.assert_location(id)?;
        let _lock = self.lock()?;
        let mut m = self.load(id)?;
        if truth(&m["cleaned_at"]) {
            return Err(Error::lane("cannot attest a cleaned lane"));
        }
        let commit = resolve(&self.repo, &format!("refs/heads/{}", s(&m, "branch")))?;
        if m["verification"].is_null() {
            m["verification"] = json!({});
        }
        let record = json!({"commit":commit,"passed":passed,"evidence":evidence,"recorded_at":now(),"kind":"parent-attestation"});
        m["verification"][check] = record.clone();
        self.save(&m)?;
        Ok(json!({"id":id,"check":check,"verification":record}))
    }
    pub fn inventory(&self, id: &str, roots: &[String]) -> Result<Value> {
        self.assert_location(id)?;
        let m = self.load(id)?;
        let wt = PathBuf::from(s(&m, "worktree"));
        if wt != self.dir("worktrees").join(id)
            || m["id"] != id
            || PathBuf::from(s(&m, "repo")) != self.repo
        {
            return Err(Error::lane(
                "manifest does not match its managed lane location",
            ));
        }
        if !wt.is_dir() {
            return Err(Error::lane("artifact worktree is missing"));
        }
        let actual = absolute(Path::new(
            git(&wt, &["rev-parse", "--show-toplevel"], true)?
                .out
                .trim(),
        ))?;
        if actual != wt || git_path(&wt, "objects")? != git_path(&self.repo, "objects")? {
            return Err(Error::lane(
                "artifact worktree belongs to a different repository",
            ));
        }
        let roots = if roots.is_empty() {
            vec!["out".into(), "target".into()]
        } else {
            roots.to_vec()
        };
        for r in &roots {
            relative(r)?;
            no_links(&wt.join(relative(r)?))?;
        }
        let mut paths = BTreeMap::new();
        for ignored in [false, true] {
            let args = if ignored {
                vec![
                    "ls-files",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "-z",
                ]
            } else {
                vec!["ls-files", "--others", "--exclude-standard", "-z"]
            };
            for q in git(&wt, &args, true)?
                .out
                .split('\0')
                .filter(|q| !q.is_empty())
            {
                if roots
                    .iter()
                    .any(|r| q == normalize(r) || q.starts_with(&(normalize(r) + "/")))
                {
                    paths.insert(q.to_owned(), ignored);
                }
            }
        }
        let mut files = vec![];
        for (q, ignored) in paths {
            let source = wt.join(relative(&q)?);
            let (size, sha) = hash(&source)?;
            files.push(json!({"path":q,"size":size,"sha256":sha,"ignored":ignored}));
        }
        Ok(
            json!({"format":"lane-artifact-inventory/v1","lane":id,"cwd":p(&wt),"branch_commit":resolve(&self.repo,&format!("refs/heads/{}",s(&m,"branch")))?,"roots":roots,"files":files,"total_bytes":files.iter().map(|f|f["size"].as_u64().unwrap()).sum::<u64>()}),
        )
    }
    pub fn collect(&self, id: &str, roots: &[String], dest: &Path) -> Result<Value> {
        self.assert_location(id)?;
        let _lock = self.lock()?;
        let inv = self.inventory(id, roots)?;
        let wt = PathBuf::from(s(&inv, "cwd"));
        let dest = absolute(dest)?;
        no_links(&dest)?;
        if dest.starts_with(&wt) || dest.starts_with(self.dir("worktrees")) {
            return Err(Error::lane(
                "collection destination must be outside managed worktrees",
            ));
        }
        let collection_id = stamp();
        let folder = dest.join(format!("{id}-{collection_id}"));
        fs::create_dir_all(&dest)?;
        fs::create_dir(&folder)?;
        let receipt_path = self
            .dir("collections")
            .join(format!("{collection_id}-{id}.json"));
        let mut receipt = json!({"format":"lane-artifact-collection/v1","collection_id":collection_id,"lane":id,"source":p(&wt),"branch_commit":inv["branch_commit"],"destination":p(&folder),"created_at":now(),"status":"copying","files":[]});
        write_json(&receipt_path, &receipt)?;
        let attempt = (|| -> Result<()> {
            for entry in inv["files"].as_array().unwrap() {
                let rel = relative(&s(entry, "path"))?;
                let source = wt.join(&rel);
                let target = folder.join(&rel);
                no_links(&target)?;
                fs::create_dir_all(target.parent().unwrap())?;
                if target.exists() {
                    return Err(Error::lane("collection target already exists"));
                }
                let mut input = fs::File::open(&source)?;
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)?;
                std::io::copy(&mut input, &mut output)?;
                output.sync_all()?;
                drop(output);
                let (size, sha) = hash(&target)?;
                if size != entry["size"].as_u64().unwrap()
                    || sha != s(entry, "sha256")
                    || hash(&source)? != (size, sha.clone())
                {
                    return Err(Error::lane(format!(
                        "artifact changed during collection: {}",
                        p(&source)
                    )));
                }
                let mut record = entry.clone();
                record["destination"] = json!(p(&target));
                receipt["files"].as_array_mut().unwrap().push(record);
                write_json(&receipt_path, &receipt)?;
            }
            if resolve(
                &self.repo,
                &format!("refs/heads/{}", s(&self.load(id)?, "branch")),
            )? != s(&inv, "branch_commit")
            {
                return Err(Error::lane("lane tip changed during collection"));
            }
            Ok(())
        })();
        if let Err(e) = attempt {
            receipt["status"] = json!("failed");
            receipt["error"] = json!(e.message);
            write_json(&receipt_path, &receipt)?;
            let mut err = Error::lane(format!(
                "collection failed; originals preserved; inspect {}",
                p(&receipt_path)
            ));
            err.details =
                json!({"receipt":p(&receipt_path),"destination":p(&folder),"cause":e.message});
            return Err(err);
        }
        receipt["status"] = json!("verified");
        receipt["verified_at"] = json!(now());
        write_json(&receipt_path, &receipt)?;
        receipt["receipt"] = json!(p(&receipt_path));
        Ok(receipt)
    }
    pub fn verified_collection(
        &self,
        id: &str,
        m: &Value,
        path: Option<&Path>,
        resume: bool,
    ) -> Result<Vec<Value>> {
        let required = strings(&m["contract"]["required_artifacts"]);
        let Some(path) = path else {
            if required.is_empty() {
                return Ok(vec![]);
            }
            return Err(Error::lane(
                "cleanup requires a verified artifact collection",
            ));
        };
        no_links(path)?;
        let c = read_json(path)?;
        let wt = self.dir("worktrees").join(id);
        if c["format"] != "lane-artifact-collection/v1"
            || c["status"] != "verified"
            || c["lane"] != id
            || s(&c, "source") != p(&wt)
        {
            return Err(Error::lane(
                "collection does not match this lane or is not verified",
            ));
        }
        if c["branch_commit"] != resolve(&self.repo, &format!("refs/heads/{}", s(m, "branch")))? {
            return Err(Error::lane(
                "collection was made for a different lane tip; collect again",
            ));
        }
        let dest = absolute(Path::new(&s(&c, "destination")))?;
        if dest.starts_with(self.dir("worktrees")) {
            return Err(Error::lane(
                "collection destination overlaps managed worktrees",
            ));
        }
        no_links(&dest)?;
        let files = c["files"]
            .as_array()
            .ok_or_else(|| Error::lane("collection files must be an array"))?
            .clone();
        let mut names = vec![];
        for f in &files {
            let q = s(f, "path");
            let rel = relative(&q)?;
            if names.contains(&q) {
                return Err(Error::lane("duplicate collection path"));
            }
            names.push(q.clone());
            let destination = dest.join(&rel);
            if s(f, "destination") != p(&destination) {
                return Err(Error::lane("collection destination does not match path"));
            }
            let expected = (
                f["size"]
                    .as_u64()
                    .ok_or_else(|| Error::lane("invalid collection size"))?,
                s(f, "sha256"),
            );
            if hash(&destination)? != expected {
                return Err(Error::lane(format!(
                    "collected artifact failed verification: {}",
                    p(&destination)
                )));
            }
            let source = wt.join(&rel);
            if source.exists() {
                if hash(&source)? != expected {
                    return Err(Error::lane(format!(
                        "source artifact changed after collection: {q}"
                    )));
                }
            } else if !(strings(&m["cleanup_progress"]["artifacts_removed"]).contains(&q)
                || (resume
                    && m["cleanup_progress"]["artifact_remove_pending"] == q
                    && s(&m["cleanup_progress"], "collection") == p(path)
                    && m["cleanup_progress"]["branch_commit"] == c["branch_commit"]))
            {
                return Err(Error::lane(format!(
                    "source artifact is missing without a cleanup record: {q}"
                )));
            }
        }
        for r in required {
            if !names
                .iter()
                .any(|q| q == &r || q.starts_with(&(r.clone() + "/")))
            {
                return Err(Error::lane(format!(
                    "required artifact was not collected: {r}"
                )));
            }
        }
        Ok(files)
    }
}

use crate::util::*;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub struct Store {
    pub repo: PathBuf,
    pub root: PathBuf,
}
impl Store {
    pub fn from_path(start: &Path) -> Result<Self> {
        let start = absolute(start)?;
        let o = git(&start, &["rev-parse", "--show-toplevel"], false)?;
        if o.code != 0 {
            return Err(Error::lane(format!(
                "not inside a Git repository: {}",
                p(&start)
            )));
        }
        let repo = absolute(Path::new(o.out.trim()))?;
        let root = repo.join(".agent-tools").join("lane");
        Ok(Self { repo, root })
    }
    pub fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    pub fn manifest_path(&self, id: &str) -> Result<PathBuf> {
        safe_id(id)?;
        Ok(self.dir("manifests").join(format!("{id}.json")))
    }
    pub fn load(&self, id: &str) -> Result<Value> {
        let v = read_json(&self.manifest_path(id)?)?;
        if v["format"] != "lane-manifest/v1" {
            return Err(Error::lane(format!(
                "unsupported manifest format for lane {id}"
            )));
        }
        Ok(v)
    }
    pub fn save(&self, m: &Value) -> Result<()> {
        write_json(&self.manifest_path(&s(m, "id"))?, m)
    }
    pub fn ids(&self, active: bool) -> Result<Vec<String>> {
        let mut ids = vec![];
        if !self.dir("manifests").exists() {
            return Ok(ids);
        }
        for e in fs::read_dir(self.dir("manifests"))? {
            let path = e?.path();
            if path.extension().is_some_and(|x| x == "json") {
                let id = path.file_stem().unwrap().to_string_lossy().into_owned();
                if !active || !truth(&self.load(&id)?["cleaned_at"]) {
                    ids.push(id);
                }
            }
        }
        ids.sort();
        Ok(ids)
    }
    pub fn ensure(&self) -> Result<()> {
        for d in [
            "worktrees",
            "manifests",
            "preflights",
            "conflicts",
            "receipts",
            "plans",
            "replays",
            "collections",
        ] {
            fs::create_dir_all(self.dir(d))?;
        }
        let q = self.root.join("config.json");
        let mut config = if q.exists() {
            read_json(&q)?
        } else {
            json!({"format":"lane-project/v1","created_at":now()})
        };
        config.as_object_mut().unwrap().remove("runner");
        config["version"] = json!(3);
        config["agent_execution"] = json!("external");
        config["interface"] = json!("agent-json");
        if !q.exists() || read_json(&q)? != config {
            write_json(&q, &config)?;
        }
        let exclude = git_path(&self.repo, "info/exclude")?;
        fs::create_dir_all(exclude.parent().unwrap())?;
        let existing = fs::read_to_string(&exclude).unwrap_or_default();
        if !existing.lines().any(|s| s.trim() == "/.agent-tools/") {
            let mut f = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(exclude)?;
            if !existing.is_empty() && !existing.ends_with(['\n', '\r']) {
                f.write_all(b"\n")?;
            }
            f.write_all(b"/.agent-tools/\n")?;
        }
        Ok(())
    }
    pub fn lock(&self) -> Result<Lock> {
        Lock::acquire(&self.root.join("integration.lock"))
    }
    pub fn spawn(
        &self,
        id: &str,
        base: &str,
        custom_branch: Option<&str>,
        expected: &[String],
        contract: Option<&Value>,
    ) -> Result<Value> {
        safe_id(id)?;
        if let Some(c) = contract {
            crate::artifacts::validate_contract(c)?;
        }
        self.ensure()?;
        if self.manifest_path(id)?.exists() {
            return Err(Error::lane(format!("lane already exists: {id}")));
        }
        let b = custom_branch
            .map(str::to_owned)
            .unwrap_or_else(|| format!("lane/{id}"));
        if git(
            &self.repo,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}"),
            ],
            false,
        )?
        .code
            == 0
        {
            return Err(Error::lane(format!("branch already exists: {b}")));
        }
        let base_commit = resolve(&self.repo, base)?;
        let wt = self.dir("worktrees").join(id);
        if wt.exists() {
            return Err(Error::lane(format!(
                "worktree path already exists: {}",
                p(&wt)
            )));
        }
        git(
            &self.repo,
            &["worktree", "add", "-b", &b, &p(&wt), &base_commit],
            true,
        )?;
        let mut m = json!({"format":"lane-manifest/v1","version":1,"id":id,"created_at":now(),"repo":p(&self.repo),"parent_branch":branch(&self.repo)?,"base":base,"base_commit":base_commit,"branch":b,"worktree":p(&wt),"expected_paths":expected.iter().map(|s|normalize(s)).filter(|s|!s.is_empty()).collect::<Vec<_>>(),"integrated_at":null,"integrated_commit":null});
        if let Some(c) = contract {
            m["contract"] = c.clone();
        }
        self.save(&m)?;
        Ok(m)
    }
    pub fn context(&self, id: &str) -> Result<Value> {
        let m = self.load(id)?;
        let status = self.status(id)?;
        let mut c = json!({"format":"lane-agent-context/v1","id":id,"repo":p(&self.repo),"cwd":m["worktree"],"branch":m["branch"],"base_commit":m["base_commit"],"expected_paths":strings(&m["expected_paths"]),"manifest":p(&self.manifest_path(id)?),"state":status["state"],"worktree_exists":status["worktree_exists"],"constraints":{"worktree_only":true,"commit_to_lane_branch":true,"branch_switch":false,"rebase":false,"merge_parent":false,"push":false}});
        if truth(&m["replay_of"]) {
            c["replay"] = json!({"of":m["replay_of"],"source_branch":m["source_branch"],"source_branch_commit":m["source_branch_commit"],"source_base_commit":m["source_base_commit"],"artifact":m["replay_context"]});
        }
        if m.get("contract").is_some() {
            c["contract"] = m["contract"].clone();
        }
        Ok(c)
    }
    pub fn validate(&self, m: &Value) -> Result<Value> {
        let mut errors = vec![];
        let mut warnings = vec![];
        let base = s(m, "base_commit");
        let b = s(m, "branch");
        let base_commit = match resolve(&self.repo, &base) {
            Ok(x) => x,
            Err(e) => {
                errors.push(e.message);
                base
            }
        };
        let tip = match resolve(&self.repo, &format!("refs/heads/{b}")) {
            Ok(x) => x,
            Err(e) => {
                errors.push(e.message);
                String::new()
            }
        };
        let mut paths = vec![];
        let mut unexpected = vec![];
        let mut count = 0;
        if !base_commit.is_empty() && !tip.is_empty() {
            if !ancestor(&self.repo, &base_commit, &tip)? {
                errors.push("lane branch no longer descends from its pinned base_commit".into());
            }
            count = git(
                &self.repo,
                &["rev-list", "--count", &format!("{base_commit}..{tip}")],
                false,
            )?
            .out
            .trim()
            .parse::<u64>()
            .unwrap_or(0);
            paths = changed(&self.repo, &base_commit, &tip)?;
            let patterns = strings(&m["expected_paths"]);
            if patterns.is_empty() {
                warnings
                    .push("expected_paths is empty; changed-path admission is unrestricted".into());
            } else {
                unexpected = paths
                    .iter()
                    .filter(|p| !allowed(p, &patterns))
                    .cloned()
                    .collect();
                if !unexpected.is_empty() {
                    errors.push(format!(
                        "changed paths outside expected_paths: {}",
                        unexpected.join(", ")
                    ));
                }
            }
        }
        let wt = PathBuf::from(s(m, "worktree"));
        let mut dirty = Value::Null;
        let mut wb = Value::Null;
        if !wt.exists() {
            warnings.push(format!("worktree is missing: {}", p(&wt)));
        } else {
            let d = !git(&wt, &["status", "--porcelain"], true)?
                .out
                .trim()
                .is_empty();
            dirty = json!(d);
            let actual = branch(&wt)?;
            wb = json!(actual);
            if d {
                errors.push("lane worktree has uncommitted changes".into());
            }
            if actual != b {
                errors.push(format!(
                    "lane worktree is on unexpected branch: {}",
                    if actual.is_empty() {
                        "<detached>"
                    } else {
                        &actual
                    }
                ));
            }
        }
        crate::artifacts::contract_errors(self, m, &tip, &mut errors)?;
        Ok(
            json!({"valid":errors.is_empty(),"errors":errors,"warnings":warnings,"base_commit":base_commit,"branch_commit":tip,"changed_paths":paths,"unexpected_paths":unexpected,"commit_count":count,"has_changes":!paths.is_empty(),"worktree_dirty":dirty,"worktree_branch":wb}),
        )
    }
    pub fn status(&self, id: &str) -> Result<Value> {
        let m = self.load(id)?;
        if truth(&m["cleanup_progress"]) && !truth(&m["cleanup_progress"]["completed_at"]) {
            let observed = self.observed_cleanup(id)?;
            return Ok(
                json!({"id":id,"branch":m["branch"],"worktree":m["worktree"],"worktree_exists":observed["path_exists"],"dirty":null,"ahead":0,"valid":false,"integrated":true,"changed_paths":m.get("integrated_changed_paths").cloned().unwrap_or(json!([])),"errors":["cleanup is incomplete; inspect cleanup_progress and observed state"],"warnings":[],"state":"cleanup-partial","cleanup_progress":m["cleanup_progress"],"observed":observed}),
            );
        }
        if truth(&m["cleaned_at"]) {
            return Ok(
                json!({"id":id,"branch":m["branch"],"worktree":m["worktree"],"worktree_exists":false,"dirty":null,"ahead":0,"valid":true,"integrated":true,"changed_paths":m.get("integrated_changed_paths").cloned().unwrap_or(json!([])),"errors":[],"warnings":["lane was cleaned"],"state":"cleaned"}),
            );
        }
        let v = self.validate(&m)?;
        let tip = s(&v, "branch_commit");
        let ahead = v["commit_count"].as_u64().unwrap_or(0);
        let integrated = ahead > 0
            && !tip.is_empty()
            && ancestor(&self.repo, &tip, &resolve(&self.repo, "HEAD")?)?;
        let state = if v["worktree_dirty"] == true {
            "dirty"
        } else if integrated {
            "integrated"
        } else if ahead == 0 {
            "empty"
        } else {
            "active"
        };
        Ok(
            json!({"id":id,"branch":m["branch"],"worktree":m["worktree"],"worktree_exists":Path::new(&s(&m,"worktree")).exists(),"dirty":v["worktree_dirty"],"ahead":ahead,"valid":v["valid"],"integrated":integrated,"changed_paths":v["changed_paths"],"errors":v["errors"],"warnings":v["warnings"],"state":state}),
        )
    }
    pub fn brief_status(&self, m: &Value, head: &mut Option<String>) -> Result<Value> {
        let id = s(m, "id");
        let pending =
            truth(&m["cleanup_progress"]) && !truth(&m["cleanup_progress"]["completed_at"]);
        if truth(&m["cleaned_at"]) {
            return Ok(
                json!({"id":id,"state":if pending{"cleanup-partial"}else{"cleaned"},"dirty":null,"integrated":true,"tip":m["cleaned_branch_commit"]}),
            );
        }
        let tip = resolve(&self.repo, &format!("refs/heads/{}", s(m, "branch")))?;
        let wt = PathBuf::from(s(m, "worktree"));
        let dirty = if wt.exists() {
            json!(
                !git(&wt, &["status", "--porcelain"], true)?
                    .out
                    .trim()
                    .is_empty()
            )
        } else {
            Value::Null
        };
        let ahead = git(
            &self.repo,
            &[
                "rev-list",
                "--count",
                &format!("{}..{tip}", s(m, "base_commit")),
            ],
            true,
        )?
        .out
        .trim()
        .parse::<u64>()
        .map_err(|_| Error::lane("invalid commit count"))?;
        let integrated = if ahead > 0 {
            if head.is_none() {
                *head = Some(resolve(&self.repo, "HEAD")?);
            }
            ancestor(&self.repo, &tip, head.as_ref().unwrap())?
        } else {
            false
        };
        let state = if pending {
            "cleanup-partial"
        } else if dirty == true {
            "dirty"
        } else if integrated {
            "integrated"
        } else if ahead == 0 {
            "empty"
        } else {
            "active"
        };
        Ok(json!({"id":id,"state":state,"dirty":dirty,"integrated":integrated,"tip":tip}))
    }
    pub fn diff(&self, a: &str, b: &str, max: usize) -> Result<String> {
        let o = git(
            &self.repo,
            &[
                "diff",
                "--no-ext-diff",
                "--no-color",
                "--unified=3",
                a,
                b,
                "--",
            ],
            true,
        )?
        .out;
        let lines: Vec<_> = o.lines().collect();
        let mut out = lines
            .iter()
            .take(max)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        if lines.len() > max {
            out.push_str(&format!(
                "\n... truncated ({} lines omitted) ...",
                lines.len() - max
            ));
        }
        Ok(out)
    }
    pub fn packet(&self, id: &str, pf: Option<&Value>) -> Result<PathBuf> {
        let m = self.load(id)?;
        let preflight = match pf {
            Some(v) => v.clone(),
            None => self.preflight(id, "HEAD", false)?,
        };
        let packet = json!({"format":"lane-conflict-packet/v1","lane":id,"created_at":now(),"base_commit":preflight["base_commit"],"target_commit":preflight["target_commit"],"branch_commit":preflight["branch_commit"],"conflict_paths":preflight["conflict_paths"],"changed_paths":preflight["changed_paths"],"expected_paths":strings(&m["expected_paths"]),"preflight_output":preflight["output"],"lane_diff":self.diff(&s(&preflight,"base_commit"),&s(&preflight,"branch_commit"),240)?,"resolution_rule":"Resolve on a fresh worktree from the current parent tip; do not discard either side with -Xours/-Xtheirs."});
        let path = self.dir("conflicts").join(format!("{}-{id}.json", stamp()));
        write_json(&path, &packet)?;
        Ok(path)
    }
    pub fn preflight(&self, id: &str, target: &str, write: bool) -> Result<Value> {
        self.ensure()?;
        let v = self.validate(&self.load(id)?)?;
        let target_commit = resolve(&self.repo, target)?;
        let tip = s(&v, "branch_commit");
        let mut result = json!({"format":"lane-preflight/v1","lane":id,"checked_at":now(),"target":target,"target_commit":target_commit,"status":if v["valid"]==true{"pending"}else{"invalid"},"conflict_paths":[],"merged_tree":null,"output":""});
        for k in [
            "base_commit",
            "branch_commit",
            "changed_paths",
            "unexpected_paths",
            "commit_count",
            "worktree_dirty",
            "worktree_branch",
            "warnings",
            "errors",
        ] {
            result[k] = v[k].clone();
        }
        if v["valid"] == true {
            if v["commit_count"] == 0 {
                result["status"] = json!("empty");
            } else if ancestor(&self.repo, &tip, &target_commit)? {
                result["status"] = json!("already-integrated");
            } else {
                let base = git(&self.repo, &["merge-base", &target_commit, &tip], true)?
                    .out
                    .trim()
                    .to_owned();
                if base != s(&v, "base_commit") {
                    result["warnings"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!(format!(
                            "computed merge-base differs from pinned base: {base}"
                        )));
                }
                let merged = merge_tree(&self.repo, &target_commit, &tip)?;
                for k in ["status", "merged_tree", "conflict_paths", "output"] {
                    result[k] = merged[k].clone();
                }
                if result["status"] == "conflict" && strings(&result["conflict_paths"]).is_empty() {
                    let tp = changed(&self.repo, &s(&v, "base_commit"), &target_commit)?;
                    let mut conflicts: Vec<_> = strings(&v["changed_paths"])
                        .into_iter()
                        .filter(|p| tp.contains(p))
                        .collect();
                    conflicts.sort();
                    result["conflict_paths"] = json!(conflicts);
                }
            }
        }
        if write {
            let artifact = self
                .dir("preflights")
                .join(format!("{}-{id}.json", stamp()));
            write_json(&artifact, &result)?;
            result["artifact"] = json!(p(&artifact));
            if result["status"] == "conflict" {
                result["conflict_packet"] = json!(p(&self.packet(id, Some(&result))?));
            }
        }
        Ok(result)
    }
    pub fn plan(&self, ids: &[String], target: &str) -> Result<Value> {
        self.ensure()?;
        let ids = if ids.is_empty() {
            self.ids(true)?
        } else {
            ids.to_vec()
        };
        let mut acc = resolve(&self.repo, target)?;
        let mut steps = vec![];
        let mut stopped = false;
        for id in &ids {
            let v = self.validate(&self.load(id)?)?;
            let tip = s(&v, "branch_commit");
            let mut step = json!({"lane":id,"from_commit":acc,"branch_commit":tip,"status":if v["valid"]==true{"pending"}else{"invalid"},"errors":v["errors"],"warnings":v["warnings"],"conflict_paths":[],"synthetic_commit":null});
            if v["valid"] != true {
                steps.push(step);
                stopped = true;
                break;
            }
            if ancestor(&self.repo, &tip, &acc)? {
                step["status"] = json!("already-integrated");
                step["synthetic_commit"] = json!(acc);
                steps.push(step);
                continue;
            }
            let merged = merge_tree(&self.repo, &acc, &tip)?;
            step["status"] = merged["status"].clone();
            step["conflict_paths"] = merged["conflict_paths"].clone();
            if merged["status"] != "clean" || merged["merged_tree"].is_null() {
                steps.push(step);
                stopped = true;
                break;
            }
            acc = git_input(
                &self.repo,
                &[
                    "commit-tree",
                    &s(&merged, "merged_tree"),
                    "-p",
                    &acc,
                    "-p",
                    &tip,
                ],
                true,
                Some(&format!("lane plan: {id}\n")),
                true,
            )?
            .out
            .trim()
            .to_owned();
            step["synthetic_commit"] = json!(acc);
            steps.push(step);
        }
        let mut plan = json!({"format":"lane-integration-plan/v1","created_at":now(),"target":target,"target_commit":resolve(&self.repo,target)?,"lanes":ids,"status":if stopped{"blocked"}else{"clean"},"steps":steps,"final_synthetic_commit":acc});
        let artifact = self.dir("plans").join(format!("{}.json", stamp()));
        write_json(&artifact, &plan)?;
        plan["artifact"] = json!(p(&artifact));
        Ok(plan)
    }
    pub fn integrate_unlocked(&self, id: &str) -> Result<Value> {
        self.ensure()?;
        if branch(&self.repo)?.is_empty() {
            return Err(Error::lane("cannot integrate into a detached HEAD"));
        }
        if !git(&self.repo, &["status", "--porcelain"], true)?
            .out
            .trim()
            .is_empty()
        {
            return Err(Error::lane(
                "parent worktree is not clean; commit/stash unrelated changes before integration",
            ));
        }
        let pf = self.preflight(id, "HEAD", true)?;
        let before = resolve(&self.repo, "HEAD")?;
        if before != s(&pf, "target_commit") {
            return Err(Error::lane(
                "parent HEAD changed after preflight; retry integration",
            ));
        }
        if pf["status"] == "empty" {
            return Ok(
                json!({"format":"lane-integration-receipt/v1","lane":id,"status":"empty","commit":before,"preflight":pf["artifact"]}),
            );
        }
        if pf["status"] == "already-integrated" {
            return Ok(
                json!({"format":"lane-integration-receipt/v1","lane":id,"status":"already-integrated","commit":before}),
            );
        }
        if pf["status"] != "clean" {
            return Err(Error::blocked(id, &s(&pf, "status")));
        }
        let mut m = self.load(id)?;
        let tip = s(&pf, "branch_commit");
        if resolve(&self.repo, "HEAD")? != before {
            return Err(Error::lane(
                "parent HEAD changed after preflight; retry integration",
            ));
        }
        let args = ["merge", "--no-ff", "--no-edit", &tip];
        let o = git(&self.repo, &args, false)?;
        if o.code != 0 {
            let _ = git(&self.repo, &["merge", "--abort"], false);
            return Err(git_error(&self.repo, &args, &o));
        }
        let after = resolve(&self.repo, "HEAD")?;
        let mut receipt = json!({"format":"lane-integration-receipt/v1","lane":id,"integrated_at":now(),"status":"integrated","parent_before":before,"parent_after":after,"branch":m["branch"],"branch_commit":tip,"changed_paths":pf["changed_paths"]});
        let artifact = self.dir("receipts").join(format!("{}-{id}.json", stamp()));
        write_json(&artifact, &receipt)?;
        receipt["artifact"] = json!(p(&artifact));
        m["integrated_at"] = receipt["integrated_at"].clone();
        m["integrated_commit"] = json!(after);
        m["integrated_branch_commit"] = json!(tip);
        m["integrated_changed_paths"] = pf["changed_paths"].clone();
        self.save(&m)?;
        Ok(receipt)
    }
    pub fn replay(&self, id: &str, new_id: Option<&str>, target: &str) -> Result<Value> {
        let source = self.load(id)?;
        let v = self.validate(&source)?;
        let tip = s(&v, "branch_commit");
        if tip.is_empty() {
            return Err(Error::lane(format!("cannot replay unresolved lane: {id}")));
        }
        let mut dest = new_id
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{id}-replay"));
        if new_id.is_none() && self.manifest_path(&dest)?.exists() {
            dest = format!("{id}-replay-{}", stamp().replace('-', ""));
        }
        safe_id(&dest)?;
        let diff = self.diff(&s(&source, "base_commit"), &tip, 180)?;
        let contract = source.get("contract").map(|c| {
            let mut c = c.clone();
            c["frozen"] = json!(false);
            c.as_object_mut().unwrap().remove("frozen_commit");
            c
        });
        let mut created = self.spawn(
            &dest,
            target,
            None,
            &strings(&source["expected_paths"]),
            contract.as_ref(),
        )?;
        created["replay_of"] = json!(id);
        created["source_branch"] = source["branch"].clone();
        created["source_branch_commit"] = json!(tip);
        created["source_base_commit"] = source["base_commit"].clone();
        let path = self.dir("replays").join(format!("{dest}.json"));
        write_json(
            &path,
            &json!({"format":"lane-replay-context/v1","lane":dest,"replay_of":id,"source_branch":source["branch"],"source_branch_commit":tip,"source_base_commit":source["base_commit"],"source_diff":diff}),
        )?;
        created["replay_context"] = json!(p(&path));
        self.save(&created)?;
        Ok(created)
    }
}

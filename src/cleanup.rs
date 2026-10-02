use crate::{core::Store, util::*};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

impl Store {
    pub fn assert_location(&self, id: &str) -> Result<()> {
        safe_id(id)?;
        for q in [
            self.dir("worktrees").join(id),
            self.manifest_path(id)?,
            self.root.join("integration.lock"),
        ] {
            no_links(&q)?;
        }
        Ok(())
    }
    pub fn records(&self) -> Result<Vec<Value>> {
        let o = git(&self.repo, &["worktree", "list", "--porcelain", "-z"], true)?.out;
        let mut records = vec![];
        for block in o.split("\0\0") {
            let mut record = json!({});
            for field in block.split('\0').filter(|s| !s.is_empty()) {
                let (k, v) = field.split_once(' ').unwrap_or((field, ""));
                record[k] = json!(v);
            }
            if !record.as_object().unwrap().is_empty() {
                records.push(record);
            }
        }
        Ok(records)
    }
    pub fn observed_cleanup(&self, id: &str) -> Result<Value> {
        self.assert_location(id)?;
        let m = self.load(id)?;
        let wt = self.dir("worktrees").join(id);
        let registered = self
            .records()?
            .iter()
            .any(|r| absolute(Path::new(&s(r, "worktree"))).ok().as_ref() == Some(&wt));
        let files = if wt.is_dir() {
            walk(&wt)?
                .iter()
                .map(|q| p(q.strip_prefix(&wt).unwrap()))
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        let b = format!("refs/heads/{}", s(&m, "branch"));
        let branch_exists =
            git(&self.repo, &["show-ref", "--verify", "--quiet", &b], false)?.code == 0;
        let mut progress = m.get("cleanup_progress").cloned().unwrap_or(json!({}));
        if let Some(fields) = progress.as_object_mut() {
            fields.remove("observed");
        }
        Ok(
            json!({"path_exists":wt.exists(),"git_registered":registered,"files_remaining":files,"branch_exists":branch_exists,"progress":progress,"next_action":if !registered&&wt.is_dir()&&files.is_empty(){"stop processes holding the directory, then retry clean with --resume"}else if registered&&!wt.exists(){"inspect missing registered worktree; restore it before cleanup"}else if !registered&&!wt.exists(){"retry clean to finish recording progress or deleting the branch"}else{"inspect remaining files and Git state; preserve uncollected data before retry"}}),
        )
    }
    pub fn cleanup_check(
        &self,
        id: &str,
        delete: bool,
        collection: Option<&Path>,
        resume: bool,
    ) -> Value {
        let mut result = json!({"id":id,"status":"blocked","actions":[],"reasons":[]});
        let attempt = (|| -> Result<()> {
            self.assert_location(id)?;
            let m = self.load(id)?;
            let wt = self.dir("worktrees").join(id);
            let b = s(&m, "branch");
            result["worktree"] = json!(p(&wt));
            result["branch"] = json!(b);
            if m["id"] != id
                || PathBuf::from(s(&m, "repo")) != self.repo
                || PathBuf::from(s(&m, "worktree")) != wt
            {
                return Err(Error::lane(
                    "refusing cleanup: manifest does not match its managed lane location",
                ));
            }
            let br = format!("refs/heads/{b}");
            if b.is_empty() || b.starts_with('-') {
                return Err(Error::lane("refusing cleanup: invalid lane branch"));
            }
            git(&self.repo, &["check-ref-format", &br], true)?;
            if branch(&self.repo)?.is_empty() {
                return Err(Error::lane(
                    "refusing cleanup: parent HEAD must be attached to a branch",
                ));
            }
            let head = resolve(&self.repo, "HEAD")?;
            result["target_commit"] = json!(head);
            let records = self.records()?;
            let registered: Vec<_> = records
                .iter()
                .filter(|r| absolute(Path::new(&s(r, "worktree"))).ok().as_ref() == Some(&wt))
                .collect();
            if records.iter().any(|r| {
                r["branch"] == br
                    && absolute(Path::new(&s(r, "worktree"))).ok().as_ref() != Some(&wt)
            }) {
                return Err(Error::lane(
                    "refusing cleanup: lane branch is checked out in another worktree",
                ));
            }
            let exists = wt.exists();
            if !registered.is_empty() && !exists {
                return Err(Error::lane(
                    "refusing cleanup: registered worktree is missing; inspect Git worktree state",
                ));
            }
            let pending = truth(&m["cleanup_progress"]["worktree_remove_started"]);
            let mut remove_empty = false;
            if exists {
                if truth(&m["cleaned_at"]) {
                    return Err(Error::lane(
                        "refusing cleanup: worktree reappeared after cleanup",
                    ));
                }
                if registered.len() != 1 || !wt.is_dir() {
                    if resume
                        && pending
                        && registered.is_empty()
                        && wt.is_dir()
                        && fs::read_dir(&wt)?.next().is_none()
                    {
                        remove_empty = true;
                    } else {
                        return Err(Error::lane(
                            "refusing cleanup: path is not the registered lane worktree; inspect cleanup progress",
                        ));
                    }
                }
                if !remove_empty {
                    let rec = registered[0];
                    if rec.get("locked").is_some() || rec.get("prunable").is_some() {
                        return Err(Error::lane(
                            "refusing cleanup: worktree is locked or prunable",
                        ));
                    }
                    if rec["branch"] != br {
                        return Err(Error::lane(
                            "refusing cleanup: lane worktree is on an unexpected branch or detached HEAD",
                        ));
                    }
                    let actual = absolute(Path::new(
                        git(&wt, &["rev-parse", "--show-toplevel"], true)?
                            .out
                            .trim(),
                    ))?;
                    if actual != wt || git_path(&wt, "objects")? != git_path(&self.repo, "objects")?
                    {
                        return Err(Error::lane(
                            "refusing cleanup: worktree belongs to a different repository",
                        ));
                    }
                    if git(&wt, &["symbolic-ref", "--quiet", "HEAD"], true)?
                        .out
                        .trim()
                        != br
                    {
                        return Err(Error::lane(
                            "refusing cleanup: lane worktree is on an unexpected branch",
                        ));
                    }
                    let gd = PathBuf::from(
                        git(&wt, &["rev-parse", "--absolute-git-dir"], true)?
                            .out
                            .trim(),
                    );
                    for marker in [
                        "MERGE_HEAD",
                        "CHERRY_PICK_HEAD",
                        "REVERT_HEAD",
                        "rebase-merge",
                        "rebase-apply",
                        "sequencer",
                        "BISECT_LOG",
                    ] {
                        if gd.join(marker).exists() {
                            return Err(Error::lane(format!(
                                "refusing cleanup: unfinished Git operation: {marker}"
                            )));
                        }
                    }
                }
            }
            let check = git(&self.repo, &["show-ref", "--verify", "--quiet", &br], false)?;
            if check.code != 0 && check.code != 1 {
                return Err(git_error(&self.repo, &["show-ref", &br], &check));
            }
            if check.code == 1 {
                if !exists
                    && registered.is_empty()
                    && truth(&m["cleaned_at"])
                    && m["branch_deleted"] == true
                {
                    result["status"] = json!("already-cleaned");
                    return Ok(());
                }
                return Err(Error::lane(
                    "refusing cleanup: lane branch is missing without a completed cleanup record",
                ));
            }
            if m["branch_deleted"] == true {
                return Err(Error::lane(
                    "refusing cleanup: lane branch reappeared after deletion",
                ));
            }
            let tip = resolve(&self.repo, &br)?;
            result["branch_commit"] = json!(tip);
            if truth(&m["cleaned_at"]) {
                let recorded = if truth(&m["cleaned_branch_commit"]) {
                    s(&m, "cleaned_branch_commit")
                } else {
                    s(&m, "integrated_branch_commit")
                };
                if recorded != tip {
                    return Err(Error::lane(
                        "refusing cleanup: retained branch differs from the recorded cleanup tip",
                    ));
                }
            }
            if pending && m["cleanup_progress"]["branch_commit"] != tip {
                return Err(Error::lane(
                    "refusing cleanup: lane branch changed since partial cleanup",
                ));
            }
            if !ancestor(&self.repo, &tip, &head)? {
                return Err(Error::lane(
                    "refusing cleanup: lane branch is not integrated into current HEAD",
                ));
            }
            let mut contract_errors = vec![];
            crate::artifacts::contract_errors(self, &m, &tip, &mut contract_errors)?;
            if !contract_errors.is_empty() {
                return Err(Error::lane(contract_errors.join("; ")));
            }
            let files = if !truth(&m["cleaned_at"]) {
                self.verified_collection(id, &m, collection, resume)?
            } else {
                vec![]
            };
            let names: Vec<_> = files.iter().map(|f| s(f, "path")).collect();
            if exists && !remove_empty {
                if resolve(&wt, "HEAD")? != tip {
                    return Err(Error::lane(
                        "refusing cleanup: worktree HEAD differs from the lane branch tip",
                    ));
                }
                walk(&wt)?;
                let dirty = git(
                    &wt,
                    &[
                        "--no-optional-locks",
                        "status",
                        "--porcelain",
                        "-z",
                        "--untracked-files=all",
                        "--ignored=matching",
                        "--ignore-submodules=none",
                    ],
                    true,
                )?
                .out;
                // Git folds ignored directories. Expand those to individual ignored files.
                for record in dirty.split('\0').filter(|s| !s.is_empty()) {
                    if record.len() < 3 {
                        return Err(Error::lane("invalid Git status record"));
                    }
                    let tag = &record[..2];
                    let q = &record[3..];
                    if tag != "!!" && tag != "??" {
                        return Err(Error::lane(
                            "refusing cleanup: lane worktree has uncommitted, untracked, or ignored files",
                        ));
                    }
                    if q.ends_with('/') {
                        let prefix = q;
                        let listed = git(
                            &wt,
                            &[
                                "ls-files",
                                "--others",
                                "--ignored",
                                "--exclude-standard",
                                "-z",
                            ],
                            true,
                        )?
                        .out;
                        let under: Vec<_> = listed
                            .split('\0')
                            .filter(|s| !s.is_empty() && s.starts_with(prefix))
                            .collect();
                        if under.is_empty() || under.iter().any(|s| !names.iter().any(|n| n == s)) {
                            return Err(Error::lane(
                                "refusing cleanup: lane worktree has uncommitted, untracked, or ignored files; collect remaining files first",
                            ));
                        }
                    } else if !names.iter().any(|n| n == q) {
                        return Err(Error::lane(
                            "refusing cleanup: lane worktree has uncommitted, untracked, or ignored files; collect remaining files first",
                        ));
                    }
                }
            }
            let mut actions = vec![];
            if !files.is_empty() {
                actions.push("remove-collected-artifacts");
            }
            if remove_empty {
                actions.push("remove-empty-directory");
            } else if exists {
                actions.push("remove-worktree");
            }
            if delete {
                actions.push("delete-branch");
            }
            if !truth(&m["cleaned_at"]) {
                actions.push("record-cleanup");
            }
            result["status"] = json!(if actions.is_empty() {
                "already-cleaned"
            } else {
                "ready"
            });
            result["actions"] = json!(actions);
            Ok(())
        })();
        if let Err(e) = attempt {
            result["reasons"] = json!([e.message]);
            result["error_code"] = json!(e.code);
        }
        result
    }
    pub fn clean(
        &self,
        id: &str,
        delete: bool,
        collection: Option<&Path>,
        resume: bool,
    ) -> Result<Value> {
        self.assert_location(id)?;
        self.load(id)?;
        let _lock = self.lock()?;
        let check = self.cleanup_check(id, delete, collection, resume);
        if check["status"] == "blocked" {
            let mut e = Error::lane(strings(&check["reasons"]).join("; "));
            e.details = json!({"cleanup":check,"observed":self.observed_cleanup(id).unwrap_or(Value::Null)});
            return Err(e);
        }
        let mut m = self.load(id)?;
        if check["status"] == "already-cleaned" {
            return Ok(m);
        }
        let wt = self.dir("worktrees").join(id);
        let tip = s(&check, "branch_commit");
        let br = format!("refs/heads/{}", s(&m, "branch"));
        let verify = || -> Result<()> {
            if resolve(&self.repo, "HEAD")? != s(&check, "target_commit") {
                return Err(Error::lane(
                    "refusing cleanup: parent HEAD changed; retry cleanup",
                ));
            }
            if resolve(&self.repo, &br)? != tip {
                return Err(Error::lane(
                    "refusing cleanup: lane branch changed; retry cleanup",
                ));
            }
            Ok(())
        };
        let actions = strings(&check["actions"]);
        let attempt = (|| -> Result<()> {
            verify()?;
            if m["cleanup_progress"].is_null() {
                m["cleanup_progress"] = json!({"started_at":now(),"branch_commit":tip,"target_commit":check["target_commit"],"artifacts_removed":[]});
            }
            if let Some(c) = collection {
                m["cleanup_progress"]["collection"] = json!(p(c));
            }
            self.save(&m)?;
            if actions.iter().any(|s| s == "remove-collected-artifacts") {
                let files = self.verified_collection(id, &m, collection, resume)?;
                for f in files {
                    let q = s(&f, "path");
                    let source = wt.join(crate::artifacts::relative(&q)?);
                    if source.exists() {
                        verify()?;
                        if hash(&source)? != (f["size"].as_u64().unwrap(), s(&f, "sha256")) {
                            return Err(Error::lane("artifact changed immediately before removal"));
                        }
                        m["cleanup_progress"]["artifact_remove_pending"] = json!(q);
                        self.save(&m)?;
                        fs::remove_file(source)?;
                        m["cleanup_progress"]["artifacts_removed"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!(q));
                        m["cleanup_progress"]["artifact_remove_pending"] = Value::Null;
                        self.save(&m)?;
                    } else if resume && m["cleanup_progress"]["artifact_remove_pending"] == q {
                        m["cleanup_progress"]["artifacts_removed"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!(q));
                        m["cleanup_progress"]["artifact_remove_pending"] = Value::Null;
                        self.save(&m)?;
                    }
                }
            }
            if actions.iter().any(|s| s == "remove-worktree") {
                verify()?;
                m["cleanup_progress"]["worktree_remove_started"] = json!(true);
                self.save(&m)?;
                git(&self.repo, &["worktree", "remove", &p(&wt)], true)?;
            }
            if actions.iter().any(|s| s == "remove-empty-directory") {
                verify()?;
                no_links(&wt)?;
                fs::remove_dir(&wt)?;
            }
            m["cleaned_at"] = if truth(&m["cleaned_at"]) {
                m["cleaned_at"].clone()
            } else {
                json!(now())
            };
            m["cleaned_branch_commit"] = json!(tip);
            m["worktree_removed"] = json!(true);
            m["branch_deleted"] = json!(false);
            m["cleanup_progress"]["worktree_removed"] = json!(true);
            self.save(&m)?;
            if delete {
                verify()?;
                git(&self.repo, &["branch", "-d", "--", &s(&m, "branch")], true)?;
                m["branch_deleted"] = json!(true);
                self.save(&m)?;
            }
            m["cleanup_progress"]["completed_at"] = json!(now());
            self.save(&m)?;
            Ok(())
        })();
        if let Err(mut e) = attempt {
            let observed = self.observed_cleanup(id).unwrap_or(Value::Null);
            m["cleanup_progress"]["last_error"] = json!(e.message);
            m["cleanup_progress"]["observed"] = observed.clone();
            let save_error = self.save(&m).err().map(|e| e.message);
            e.details = json!({"cleanup":observed,"manifest":p(&self.manifest_path(id)?),"progress_save_error":save_error});
            return Err(e);
        }
        Ok(m)
    }
    pub fn clean_many(&self, delete: bool, dry: bool, resume: bool) -> Result<Value> {
        let mut lanes = vec![];
        for id in self.ids(false)? {
            let mut c = self.cleanup_check(&id, delete, None, resume);
            if !dry && c["status"] == "ready" {
                match self.clean(&id, delete, None, resume) {
                    Ok(_) => c["status"] = json!("cleaned"),
                    Err(e) => {
                        c["status"] = json!("blocked");
                        c["reasons"] = json!([e.message]);
                        c["error_code"] = json!(e.code);
                        c["details"] = e.details;
                    }
                }
            }
            lanes.push(c);
        }
        let mut counts = json!({});
        for state in ["ready", "cleaned", "already-cleaned", "blocked"] {
            counts[state] = json!(lanes.iter().filter(|c| c["status"] == state).count());
        }
        Ok(
            json!({"format":"lane-cleanup-report/v1","dry_run":dry,"delete_branch":delete,"status":if counts["blocked"].as_u64().unwrap()>0{"blocked"}else if dry{"preview"}else{"cleaned"},"counts":counts,"lanes":lanes}),
        )
    }
}

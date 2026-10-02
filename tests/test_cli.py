"""Exercise the real executable and preserved v1 fixtures in temporary repos."""
import ctypes
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import shutil

from support import ROOT, EXE, CliStore, git, git_path, resolve, fixture_lock
SHIM = ROOT / "target/test-tools" / ("git.exe" if os.name == "nt" else "git")


class CliTests(unittest.TestCase):
    maxDiff = None
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="lane-native-")
        self.repo = Path(self.temp.name) / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-b", "main")
        git(self.repo, "config", "user.name", "Lane Tests")
        git(self.repo, "config", "user.email", "lane-tests@local.invalid")
        self.commit(self.repo, "app.txt", "alpha\nbeta\ngamma\n")
        self.fixtures = CliStore(self.repo)
        self.call("init")

    def tearDown(self):
        self.temp.cleanup()

    def call(self, *args, code=0, env=None):
        r = subprocess.run([str(EXE), "--project", str(self.repo), *map(str, args)], capture_output=True, env=env)
        self.assertEqual(r.returncode, code, r.stdout.decode("ascii") + r.stderr.decode("utf-8", errors="replace"))
        self.assertEqual(len(r.stdout.splitlines()), 1)
        self.assertEqual(r.stderr, b"")
        v = json.loads(r.stdout.decode("ascii"))
        self.assertEqual(v["format"], "lane-agent-response/v1")
        return v["data"] if v["ok"] else v["error"]

    def commit(self, repo, rel, text):
        p = repo / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8")
        git(repo, "add", "--", rel)
        git(repo, "commit", "-m", rel)
        return resolve(repo, "HEAD")

    def spawn(self, id="worker", *args):
        return Path(self.call("spawn", id, *args)["cwd"])

    def shim_env(self, mode, target):
        if not SHIM.exists() or SHIM.stat().st_mtime < (ROOT / "tests/fixtures/git_shim.rs").stat().st_mtime:
            SHIM.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(["rustc", str(ROOT / "tests/fixtures/git_shim.rs"), "-O", "-o", str(SHIM)], check=True)
        env = os.environ.copy()
        env["LANE_TEST_REAL_GIT"] = shutil.which("git")
        env["LANE_TEST_MODE"] = mode
        env["LANE_TEST_TARGET"] = str(target)
        env["LANE_TEST_MARKER"] = str(Path(self.temp.name) / "injected")
        env["PATH"] = str(SHIM.parent) + os.pathsep + env["PATH"]
        return env

    def test_integration_uses_verified_commit_when_worker_advances(self):
        wt = self.spawn("worker", "--expected", "app.txt")
        verified = self.commit(wt, "app.txt", "verified")
        receipt = self.call("integrate", "worker", env=self.shim_env("worker-after-preflight", wt))
        self.assertEqual(receipt["branch_commit"], verified)
        self.assertFalse((self.repo / "late.txt").exists())
        self.assertNotEqual(git(self.repo, "merge-base", "--is-ancestor", resolve(wt, "HEAD"), "HEAD", check=False).returncode, 0)

    def test_parent_change_after_preflight_blocks_integration(self):
        wt = self.spawn()
        tip = self.commit(wt, "app.txt", "worker")
        e = self.call("integrate", "worker", code=12, env=self.shim_env("parent-after-preflight", self.repo))
        self.assertIn("parent HEAD changed", e["message"])
        self.assertNotEqual(git(self.repo, "merge-base", "--is-ancestor", tip, "HEAD", check=False).returncode, 0)

    def test_console_interrupt_returns_single_line_json(self):
        wt = self.spawn()
        self.commit(wt, "app.txt", "worker")
        env = self.shim_env("interrupt", wt)
        options = {}
        if os.name == "nt":
            startup = subprocess.STARTUPINFO()
            startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
            startup.wShowWindow = subprocess.SW_HIDE
            options = {"creationflags":subprocess.CREATE_NEW_CONSOLE, "startupinfo":startup}
        r = subprocess.run([str(EXE), "--project", str(self.repo), "integrate", "worker"], env=env, capture_output=True, timeout=20, **options)
        self.assertEqual(r.returncode, 130, (r.stdout, r.stderr))
        self.assertEqual(len(r.stdout.splitlines()), 1)
        self.assertEqual(json.loads(r.stdout)["error"]["code"], "interrupted")
        self.assertEqual(git(self.repo,"status","--porcelain").stdout, "")
        self.assertTrue(wt.exists())

    def test_cleanup_rechecks_parent_and_worker_refs(self):
        for mode, target_parent in (("parent-before-clean", True), ("worker-before-clean", False)):
            with self.subTest(mode=mode):
                id = mode
                wt = self.spawn(id)
                target = self.repo if target_parent else wt
                marker = Path(self.temp.name) / "injected"
                if marker.exists():
                    marker.unlink()
                e = self.call("clean", id, code=12, env=self.shim_env(mode, target))
                self.assertIn("changed", e["message"])
                self.assertTrue(wt.exists())

    def test_git_status_failure_is_never_treated_as_clean(self):
        wt = self.spawn()
        env = self.shim_env("fail-status", wt)
        self.assertEqual(self.call("validate", "worker", code=13, env=env)["code"], "git_error")
        v = self.call("clean", "worker", "--dry-run", code=12, env=env)
        self.assertEqual(v["error_code"], "git_error")
        self.assertTrue(wt.exists())

    def test_interrupted_artifact_removal_requires_explicit_resume(self):
        wt = self.artifact_setup()
        c = self.call("collect", "worker", "--dest", Path(self.temp.name) / "archive")
        first = c["files"][0]
        m = self.fixtures.load("worker")
        m["cleanup_progress"] = {"branch_commit":resolve(wt,"HEAD"), "collection":c["receipt"], "artifacts_removed":[], "artifact_remove_pending":first["path"]}
        self.fixtures.save(m)
        (wt / first["path"]).unlink()
        self.call("clean", "worker", "--collection", c["receipt"], code=12)
        self.call("clean", "worker", "--collection", c["receipt"], "--resume", "--delete-branch")
        self.assertFalse(wt.exists())

    def test_advanced_tip_invalidates_collection(self):
        wt = self.artifact_setup()
        c = self.call("collect", "worker", "--dest", Path(self.temp.name) / "archive")
        git(wt, "commit", "--allow-empty", "-m", "later")
        self.call("integrate", "worker")
        e = self.call("clean", "worker", "--collection", c["receipt"], code=12)
        self.assertIn("different lane tip", e["message"])
        self.assertTrue((wt / "out/build.log").exists())

    def test_v1_manifest_fixture_can_be_read_integrated_and_cleaned(self):
        base = resolve(self.repo, "HEAD")
        wt = self.fixtures.worktrees / "legacy"
        git(self.repo, "worktree", "add", "-b", "lane/legacy", str(wt), base)
        self.fixtures.save({
            "format": "lane-manifest/v1", "version": 1, "id": "legacy",
            "created_at": "2026-01-01T00:00:00+00:00", "repo": str(self.repo),
            "parent_branch": "main", "base": "HEAD", "base_commit": base,
            "branch": "lane/legacy", "worktree": str(wt), "expected_paths": ["app.txt"],
            "integrated_at": None, "integrated_commit": None,
        })
        self.assertEqual(self.call("context", "legacy")["state"], "empty")
        self.assertTrue(self.call("validate", "legacy")["valid"])
        tip = self.commit(wt, "app.txt", "legacy work\n")
        self.assertEqual(self.call("preflight", "legacy")["status"], "clean")
        self.assertEqual(self.call("integrate", "legacy")["branch_commit"], tip)
        self.assertEqual(git(self.repo, "merge-base", "--is-ancestor", tip, "HEAD").returncode, 0)
        self.call("clean", "legacy", "--delete-branch")
        self.assertFalse(wt.exists())
        self.assertEqual(self.call("context", "legacy")["state"], "cleaned")

    def test_unicode_dot_paths_and_scope(self):
        wt = self.spawn("scope", "--expected", "./.github/", "--expected", "docs/*.md")
        self.commit(wt, ".github/settings.json", "{}")
        self.commit(wt, "docs/日本語 😀.md", "ok")
        self.assertTrue(self.call("validate", "scope")["valid"])
        self.commit(wt, "other.txt", "outside")
        invalid = self.call("validate", "scope", code=10)
        self.assertEqual(invalid["unexpected_paths"], ["other.txt"])
        self.assertEqual(self.call("preflight", "scope", code=10)["status"], "invalid")
        self.assertEqual(self.call("integrate", "scope", code=10)["code"], "blocked")

    def test_dirty_and_tag_ambiguity(self):
        wt = self.spawn()
        git(self.repo, "tag", "lane/worker")
        tip = self.commit(wt, "app.txt", "lane\n")
        self.assertEqual(self.call("validate", "worker")["branch_commit"], tip)
        (wt / "local").write_text("dirty")
        invalid = self.call("validate", "worker", code=10)
        self.assertTrue(invalid["worktree_dirty"])
        self.assertEqual(self.call("clean", "worker", "--dry-run", code=12)["status"], "blocked")

    def test_empty_and_reverted_commits_remain_in_history(self):
        for id, empty in (("empty", True), ("revert", False)):
            wt = self.spawn(id)
            if empty:
                git(wt, "commit", "--allow-empty", "-m", "empty")
            else:
                original = (wt / "app.txt").read_text()
                self.commit(wt, "app.txt", "temporary")
                self.commit(wt, "app.txt", original)
            tip = resolve(wt, "HEAD")
            self.assertFalse(self.call("validate", id)["has_changes"])
            self.assertEqual(self.call("preflight", id)["status"], "clean")
            self.assertEqual(self.call("integrate", id)["branch_commit"], tip)
            self.call("clean", id, "--delete-branch")

    def test_plan_cross_lane_conflict_is_non_destructive(self):
        a = self.spawn("a")
        b = self.spawn("b")
        self.commit(a, "app.txt", "alpha\na\ngamma\n")
        self.commit(b, "app.txt", "alpha\nb\ngamma\n")
        before = resolve(self.repo, "HEAD")
        index = git_path(self.repo, "index").read_bytes()
        refs = git(self.repo, "show-ref").stdout
        self.assertEqual(self.call("preflight", "a")["status"], "clean")
        self.assertEqual(self.call("preflight", "b")["status"], "clean")
        plan = self.call("plan", "a", "b", code=11)
        self.assertEqual(plan["steps"][1]["status"], "conflict")
        self.assertEqual(self.call("integrate-all", "a", "b", code=11)["receipts"], [])
        self.assertEqual(resolve(self.repo, "HEAD"), before)
        self.assertEqual(git_path(self.repo, "index").read_bytes(), index)
        self.assertEqual(git(self.repo, "show-ref").stdout, refs)

    def test_conflict_packet_and_replay_preserve_source(self):
        wt = self.spawn()
        tip = self.commit(wt, "app.txt", "alpha\nlane\ngamma\n")
        parent = self.commit(self.repo, "app.txt", "alpha\nparent\ngamma\n")
        pf = self.call("preflight", "worker", code=11)
        self.assertIn("app.txt", pf["conflict_paths"])
        self.assertTrue(Path(pf["conflict_packet"]).is_file())
        packet = self.call("packet", "worker")["packet"]
        self.assertIn("lane", packet["lane_diff"])
        replay = self.call("replay", "worker", "--new-id", "resolution")
        self.assertEqual(replay["base_commit"], parent)
        self.assertEqual(replay["replay"]["source_branch_commit"], tip)
        self.assertTrue(wt.exists())

    def test_bulk_history_and_idempotent_cleanup(self):
        old = self.spawn("old")
        self.call("clean", "old", "--delete-branch")
        self.spawn("fresh")
        self.assertEqual(self.call("plan")["lanes"], ["fresh"])
        self.assertEqual(self.call("integrate-all")["status"], "integrated")
        self.assertFalse(old.exists())
        result = self.call("clean", "--all", "--delete-branch")
        self.assertEqual(result["counts"]["already-cleaned"], 1)
        before = self.fixtures.manifest_path("fresh").read_bytes()
        self.call("clean", "fresh", "--delete-branch")
        self.assertEqual(self.fixtures.manifest_path("fresh").read_bytes(), before)

    def test_read_only_preview_and_bulk_partial(self):
        self.spawn("ready")
        active = self.spawn("active")
        self.commit(active, "app.txt", "unfinished")
        self.fixtures.manifest_path("broken").write_text("{")
        before = self.fixtures.manifest_path("ready").read_bytes()
        index = git_path(self.repo, "index").read_bytes()
        refs = git(self.repo, "show-ref").stdout
        v = self.call("clean", "--all", "--dry-run", "--delete-branch", code=12)
        self.assertEqual(v["counts"]["blocked"], 2)
        self.assertEqual(self.fixtures.manifest_path("ready").read_bytes(), before)
        self.assertEqual(git_path(self.repo, "index").read_bytes(), index)
        self.assertEqual(git(self.repo, "show-ref").stdout, refs)
        v = self.call("clean", "--all", "--delete-branch", code=12)
        self.assertEqual(v["counts"]["cleaned"], 1)
        self.assertTrue(active.exists())

    def test_cleanup_detached_switched_locked_and_operation(self):
        for mode in ("detached", "switched", "locked", "merge"):
            with self.subTest(mode=mode):
                wt = self.spawn(mode)
                if mode == "detached":
                    git(wt, "checkout", "--detach")
                elif mode == "switched":
                    git(wt, "checkout", "-b", "other")
                elif mode == "locked":
                    git(self.repo, "worktree", "lock", str(wt))
                else:
                    git_path(wt, "MERGE_HEAD").write_text(resolve(self.repo, "HEAD") + "\n")
                v = self.call("clean", mode, "--dry-run", code=12)
                self.assertEqual(v["status"], "blocked")
                self.assertTrue(wt.exists())
                if mode == "locked":
                    git(self.repo, "worktree", "unlock", str(wt))

    def test_manifest_and_unregistered_paths_are_preserved(self):
        wt = self.spawn()
        m = self.fixtures.load("worker")
        m["worktree"] = str(self.repo)
        self.fixtures.save(m)
        self.call("clean", "worker", code=12)
        self.assertTrue(wt.exists())
        m["worktree"] = str(wt)
        self.fixtures.save(m)
        git(self.repo, "worktree", "remove", str(wt))
        wt.mkdir()
        sentinel = wt / "keep"
        sentinel.write_text("keep")
        self.call("clean", "worker", "--resume", code=12)
        self.assertEqual(sentinel.read_text(), "keep")

    def test_external_byte_lock_blocks_native_operations(self):
        wt = self.spawn()
        with fixture_lock(self.fixtures.root / "integration.lock"):
            self.assertIn("lock busy", self.call("clean", "worker", code=12)["message"])
            self.assertIn("lock busy", self.call("integrate", "worker", code=12)["message"])
        self.assertTrue(wt.exists())

    def test_branch_deletion_failure_can_be_retried(self):
        wt = self.spawn()
        git(self.repo, "branch", "old-base")
        self.commit(wt, "app.txt", "worker")
        git(wt, "branch", "--set-upstream-to=old-base")
        self.call("integrate", "worker")
        e = self.call("clean", "worker", "--delete-branch", code=13)
        self.assertEqual(e["details"]["cleanup"]["path_exists"], False)
        self.assertFalse(wt.exists())
        self.assertTrue(self.fixtures.load("worker")["worktree_removed"])
        second = self.call("clean", "worker", "--delete-branch", code=13)
        self.assertNotIn("observed", second["details"]["cleanup"]["progress"])
        status = self.call("status", "worker")["lanes"][0]
        self.assertEqual(status["state"], "cleanup-partial")
        self.assertEqual(self.call("status", "--cleanup-pending", "--short")["lanes"][0]["id"], "worker")
        git(self.repo, "branch", "--unset-upstream", "lane/worker")
        self.call("clean", "worker", "--delete-branch")

    def artifact_setup(self):
        self.commit(self.repo, ".gitignore", "out/\ntarget/\n")
        wt = self.spawn()
        for rel in ("out/build.log", "target/release/app.exe", "out/日本語 😀.bin"):
            q = wt / rel
            q.parent.mkdir(parents=True, exist_ok=True)
            q.write_bytes(b"artifact\x00\xff")
        return wt

    def test_artifact_inventory_collection_and_verified_cleanup(self):
        wt = self.artifact_setup()
        inv = self.call("artifacts", "worker")
        self.assertEqual(len(inv["files"]), 3)
        self.call("clean", "worker", "--dry-run", code=12)
        dest = Path(self.temp.name) / "archive"
        c = self.call("collect", "worker", "--dest", dest)
        receipt = c["receipt"]
        self.assertEqual(c["status"], "verified")
        for f in c["files"]:
            self.assertEqual((wt / f["path"]).read_bytes(), Path(f["destination"]).read_bytes())
        before = self.fixtures.manifest_path("worker").read_bytes()
        self.assertEqual(self.call("clean", "worker", "--dry-run", "--collection", receipt)["status"], "ready")
        self.assertEqual(self.fixtures.manifest_path("worker").read_bytes(), before)
        self.call("clean", "worker", "--collection", receipt, "--delete-branch")
        self.assertFalse(wt.exists())
        self.assertTrue(all(Path(f["destination"]).exists() for f in c["files"]))
        self.call("clean", "worker", "--collection", receipt, "--delete-branch")

    def test_collection_source_or_destination_changes_block_deletion(self):
        wt = self.artifact_setup()
        c = self.call("collect", "worker", "--dest", Path(self.temp.name) / "archive")
        first = c["files"][0]
        original = (wt / first["path"]).read_bytes()
        (wt / first["path"]).write_bytes(b"new log")
        self.call("clean", "worker", "--collection", c["receipt"], code=12)
        self.assertTrue(wt.exists())
        (wt / first["path"]).write_bytes(original)
        Path(first["destination"]).write_bytes(b"corrupted")
        self.call("clean", "worker", "--collection", c["receipt"], code=12)
        self.assertEqual((wt / first["path"]).read_bytes(), original)

    def test_collection_cannot_hide_other_local_files_or_use_overlapping_dest(self):
        wt = self.artifact_setup()
        self.call("collect", "worker", "--dest", wt / "out/archive", code=12)
        c = self.call("collect", "worker", "--dest", Path(self.temp.name) / "archive")
        (wt / "keep-local").write_text("keep")
        self.call("clean", "worker", "--collection", c["receipt"], code=12)
        self.assertTrue((wt / "out/build.log").exists())

    def test_untracked_artifacts_can_be_collected_explicitly(self):
        wt = self.spawn()
        (wt / "out").mkdir()
        (wt / "out/build.log").write_text("untracked")
        c = self.call("collect", "worker", "--dest", Path(self.temp.name) / "archive")
        self.assertFalse(c["files"][0]["ignored"])
        self.call("clean", "worker", "--collection", c["receipt"])
        self.assertFalse(wt.exists())

    def test_contract_checks_dependencies_freeze_and_collection(self):
        self.commit(self.repo, ".gitignore", "out/\n")
        contract = Path(self.temp.name) / "contract.json"
        contract.write_text(json.dumps({"owner":"agent-a", "campaign":"build", "symbols":["api::parse"], "dependencies":[resolve(self.repo, "HEAD")], "required_checks":["tests"], "required_artifacts":["out/build.log"]}))
        wt = self.spawn("worker", "--contract", contract)
        self.assertEqual(self.call("context", "worker")["contract"]["symbols"], ["api::parse"])
        self.call("validate", "worker", code=10)
        self.call("attest", "worker", "--check", "tests", "--evidence", "test log")
        self.call("validate", "worker")
        self.call("contract", "worker", "--freeze")
        self.commit(wt, "app.txt", "later")
        v = self.call("validate", "worker", code=10)
        self.assertIn("lane tip differs from frozen_commit", v["errors"])
        self.call("attest", "worker", "--check", "tests")
        self.call("contract", "worker", "--freeze")
        self.call("integrate", "worker")
        self.call("clean", "worker", code=12)
        (wt / "out").mkdir()
        (wt / "out/build.log").write_text("verified")
        c = self.call("collect", "worker", "--dest", Path(self.temp.name) / "archive")
        self.call("clean", "worker", "--collection", c["receipt"])

    def test_status_filters_short_and_pagination(self):
        self.spawn("a", "--owner", "alice", "--campaign", "build")
        b = self.spawn("b", "--owner", "bob", "--campaign", "build")
        self.commit(b, "app.txt", "b")
        self.call("integrate", "b")
        self.spawn("history")
        self.call("clean", "history", "--delete-branch")
        v = self.call("status", "--campaign", "build", "--short", "--limit", "1", "--offset", "1")
        self.assertEqual(v["total"], 2)
        self.assertEqual(set(v["lanes"][0]), {"id","state","cwd","tip","dirty","owner"})
        self.assertEqual(v["lanes"][0]["owner"], "bob")
        self.assertEqual(self.call("status", "--active")["total"], 1)
        self.assertEqual(self.call("status", "--cleanup-pending")["total"], 2)
        self.assertEqual(self.call("status", "--unintegrated")["total"], 1)
        self.assertEqual(self.call("status", "--owner", "alice")["lanes"][0]["id"], "a")

    def test_argument_schema_and_usage_errors(self):
        schema = self.call("schema")
        self.assertEqual(schema["argument_schema"]["commands"]["spawn"]["positionals"]["min"], 1)
        self.assertEqual(schema["argument_schema"]["commands"]["clean"]["selection"]["exactly_one"], ["id", "--all"])
        for args in (("clean",), ("clean", "a", "--all"), ("collect", "a"), ("status", "--limit", "-1"), ("status", "--state", "bogus"), ("contract", "a", "--freeze", "--unfreeze"), ("spawn", "a", "--unknown")):
            self.assertEqual(self.call(*args, code=2)["code"], "cli_usage_error")

    @unittest.skipUnless(os.name == "nt", "Windows directory lock")
    def test_real_windows_directory_lock_reports_partial_and_resumes(self):
        wt = self.spawn()
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.CreateFileW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p]
        kernel.CreateFileW.restype = ctypes.c_void_p
        kernel.CloseHandle.argtypes = [ctypes.c_void_p]
        handle = kernel.CreateFileW(str(wt), 0x80000000, 3, None, 3, 0x02000000, None)
        self.assertNotEqual(handle, ctypes.c_void_p(-1).value)
        try:
            e = self.call("clean", "worker", code=13)
            observed = e["details"]["cleanup"]
            self.assertTrue(observed["path_exists"])
            self.assertFalse(observed["git_registered"])
            self.assertEqual(observed["files_remaining"], [])
            self.assertEqual(self.call("status", "worker")["lanes"][0]["state"], "cleanup-partial")
        finally:
            kernel.CloseHandle(handle)
        self.call("clean", "worker", "--resume", "--delete-branch")
        self.assertFalse(wt.exists())

    @unittest.skipUnless(os.name == "nt", "Windows junction")
    def test_junctions_are_never_collected_or_removed(self):
        wt = self.artifact_setup()
        outside = Path(self.temp.name) / "outside"
        outside.mkdir()
        (outside / "keep").write_text("keep")
        link = wt / "out/link"
        cmd = f"New-Item -ItemType Junction -Path '{link}' -Target '{outside}' | Out-Null"
        subprocess.run(["powershell", "-NoProfile", "-Command", cmd], check=True, capture_output=True)
        try:
            self.call("collect", "worker", "--dest", Path(self.temp.name) / "archive", code=12)
            self.call("clean", "worker", code=12)
            self.assertEqual((outside / "keep").read_text(), "keep")
        finally:
            link.rmdir()


if __name__ == "__main__":
    unittest.main()

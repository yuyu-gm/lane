"""Preserved v1 scenarios executed directly against the native CLI."""
from __future__ import annotations

import io
import json
import os
import subprocess
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

from support import EXIT_LANE_ERROR, EXIT_OK, EXIT_USAGE, cli_main
from support import GitError, LaneError, CliStore as LaneStore, git, git_path, resolve
from test_lifecycle import commit_file


class V1CleanupTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="lane-cleanup-tests-")
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name) / "repo"
        self.repo.mkdir()
        self.repo = self.repo.resolve()
        git(self.repo, "init", "-b", "main")
        git(self.repo, "config", "user.name", "Lane Tests")
        git(self.repo, "config", "user.email", "lane-tests@example.invalid")
        commit_file(self.repo, "app.txt", "initial\n", "initial")
        self.store = LaneStore(self.repo)
        self.store.ensure()

    def spawn(self, lane_id: str = "done") -> Path:
        return Path(self.store.spawn(lane_id)["worktree"])

    def invoke(self, *args: str) -> tuple[int, dict]:
        output = io.StringIO()
        with redirect_stdout(output):
            rc = cli_main(["--project", str(self.repo), *args])
        self.assertEqual(len(output.getvalue().splitlines()), 1)
        return rc, json.loads(output.getvalue())

    def assert_blocked(self, lane_id: str, reason: str) -> None:
        check = self.store.clean(lane_id, delete_branch=True, dry_run=True)
        self.assertEqual(check["status"], "blocked")
        self.assertIn(reason, " ".join(check["reasons"]))
        with self.assertRaises(LaneError):
            self.store.clean(lane_id, delete_branch=True)
        self.assertFalse(self.store.load(lane_id).get("cleaned_at"))

    def test_dry_run_is_read_only_and_cleanup_is_repeatable(self) -> None:
        wt = self.spawn()
        commit_file(wt, "app.txt", "completed\n", "worker")
        self.store.integrate("done")
        manifest_before = self.store.manifest_path("done").read_bytes()
        index_before = git_path(wt, "index").read_bytes()
        refs_before = git(self.repo, "show-ref").stdout
        registration_before = git(self.repo, "worktree", "list", "--porcelain").stdout
        artifacts_before = {p: p.read_bytes() for p in self.store.receipts.glob("*.json")}
        rc, response = self.invoke("clean", "done", "--dry-run", "--delete-branch")
        self.assertEqual(rc, EXIT_OK)
        self.assertEqual(response["data"]["status"], "ready")
        self.assertIn("remove-worktree", response["data"]["actions"])
        self.assertEqual(self.store.manifest_path("done").read_bytes(), manifest_before)
        self.assertEqual(git_path(wt, "index").read_bytes(), index_before)
        self.assertEqual(git(self.repo, "show-ref").stdout, refs_before)
        self.assertEqual(git(self.repo, "worktree", "list", "--porcelain").stdout, registration_before)
        rc, response = self.invoke("clean", "done", "--delete-branch")
        self.assertEqual(rc, EXIT_OK)
        self.assertEqual(response["data"]["state"], "cleaned")
        cleaned_before = self.store.manifest_path("done").read_bytes()
        rc, _ = self.invoke("clean", "done", "--delete-branch")
        self.assertEqual(rc, EXIT_OK)
        self.assertEqual(self.store.manifest_path("done").read_bytes(), cleaned_before)
        self.assertFalse(wt.exists())
        self.assertEqual({p: p.read_bytes() for p in self.store.receipts.glob("*.json")}, artifacts_before)

    def test_cleanup_can_delete_retained_branch_later(self) -> None:
        wt = self.spawn()
        self.store.clean("done")
        self.assertFalse(wt.exists())
        self.assertFalse(self.store.load("done")["branch_deleted"])
        self.store.clean("done", delete_branch=True)
        self.assertTrue(self.store.load("done")["branch_deleted"])

    def test_dirty_untracked_and_ignored_files_are_preserved(self) -> None:
        for kind in ("tracked", "untracked", "ignored"):
            with self.subTest(kind=kind):
                wt = self.spawn(kind)
                path = wt / ("app.txt" if kind == "tracked" else "local.txt")
                path.write_text("keep me", encoding="utf-8")
                if kind == "ignored":
                    with git_path(self.repo, "info/exclude").open("a", encoding="utf-8") as handle:
                        handle.write("\nlocal.txt\n")
                self.assert_blocked(kind, "uncommitted, untracked, or ignored")
                self.assertEqual(path.read_text(encoding="utf-8"), "keep me")

    def test_detached_and_switched_worktrees_are_preserved(self) -> None:
        for mode in ("detached", "switched"):
            with self.subTest(mode=mode):
                wt = self.spawn(mode)
                if mode == "detached":
                    git(wt, "checkout", "--detach")
                else:
                    git(wt, "checkout", "-b", "unrelated")
                tip = commit_file(wt, "app.txt", "other work\n", "unrelated work")
                self.assert_blocked(mode, "unexpected branch or detached HEAD")
                self.assertEqual(resolve(wt, "HEAD"), tip)

    def test_locked_worktree_is_preserved(self) -> None:
        wt = self.spawn()
        git(self.repo, "worktree", "lock", "--reason", "worker still owns this", str(wt))
        self.assert_blocked("done", "locked")
        git(self.repo, "worktree", "unlock", str(wt))

    def test_unfinished_merge_is_preserved_even_with_a_clean_index(self) -> None:
        wt = self.spawn()
        git(self.repo, "checkout", "-b", "incoming")
        git(self.repo, "commit", "--allow-empty", "-m", "pending work")
        git(self.repo, "checkout", "main")
        git(wt, "merge", "--no-ff", "--no-commit", "incoming")
        self.assertEqual(git(wt, "status", "--porcelain").stdout, "")
        self.assert_blocked("done", "unfinished Git operation")
        self.assertTrue(git_path(wt, "MERGE_HEAD").exists())

    def test_manifest_cannot_select_another_worktree(self) -> None:
        wt = self.spawn()
        other = self.spawn("other")
        manifest = self.store.load("done")
        manifest["worktree"] = str(other)
        self.store.save(manifest)
        self.assert_blocked("done", "managed lane location")
        self.assertTrue(wt.exists())
        self.assertTrue(other.exists())

    def test_missing_registration_is_not_treated_as_a_worktree(self) -> None:
        wt = self.spawn()
        git(self.repo, "worktree", "remove", str(wt))
        wt.mkdir()
        (wt / "keep.txt").write_text("unmanaged", encoding="utf-8")
        self.assert_blocked("done", "not the registered")
        self.assertTrue((wt / "keep.txt").exists())

    def test_missing_registered_worktree_is_preserved_for_inspection(self) -> None:
        wt = self.spawn()
        moved = Path(self.temp.name) / "moved"
        wt.rename(moved)
        self.assert_blocked("done", "registered worktree is missing")
        self.assertTrue(moved.exists())
        moved.rename(wt)

    def test_cleaned_branches_and_worktrees_cannot_be_reused_silently(self) -> None:
        wt = self.spawn()
        self.store.clean("done")
        commit_file(self.repo, "app.txt", "later\n", "later")
        git(self.repo, "branch", "-f", "lane/done", "HEAD")
        check = self.store.cleanup_check("done", delete_branch=True)
        self.assertEqual(check["status"], "blocked")
        self.assertIn("recorded cleanup tip", " ".join(check["reasons"]))
        self.spawn("deleted")
        self.store.clean("deleted", delete_branch=True)
        git(self.repo, "branch", "lane/deleted")
        self.assertIn("reappeared", " ".join(self.store.cleanup_check("deleted")["reasons"]))
        wt.mkdir()
        (wt / "keep.txt").write_text("new data", encoding="utf-8")
        self.assertIn("reappeared", " ".join(self.store.cleanup_check("done")["reasons"]))
        self.assertTrue((wt / "keep.txt").exists())

    def test_branch_checked_out_elsewhere_is_preserved(self) -> None:
        self.spawn()
        self.store.clean("done")
        other = Path(self.temp.name) / "other"
        git(self.repo, "worktree", "add", str(other), "lane/done")
        self.assertIn("another worktree", " ".join(self.store.cleanup_check("done", delete_branch=True)["reasons"]))
        self.assertTrue(other.exists())


    def test_cleanup_rechecks_changes_after_preview(self) -> None:
        wt = self.spawn()
        self.assertEqual(self.store.cleanup_check("done")["status"], "ready")
        commit_file(wt, "app.txt", "new work\n", "new work")
        self.assert_blocked("done", "not integrated")
        self.assertTrue(wt.exists())


    def test_cleanup_shares_the_integration_lock(self) -> None:
        wt = self.spawn()
        with self.store.integration_lock():
            with self.assertRaisesRegex(LaneError, "lock busy"):
                self.store.clean("done")
        self.assertTrue(wt.exists())

    def test_cleanup_requires_an_attached_parent(self) -> None:
        wt = self.spawn()
        git(self.repo, "checkout", "--detach")
        self.assert_blocked("done", "parent HEAD must be attached")
        self.assertTrue(wt.exists())

    def test_tag_with_lane_name_cannot_hide_unintegrated_commits(self) -> None:
        wt = self.spawn()
        git(self.repo, "tag", "lane/done")
        tip = commit_file(wt, "app.txt", "not integrated\n", "worker")
        self.assert_blocked("done", "not integrated")
        self.assertEqual(resolve(wt, "HEAD"), tip)


    def test_bulk_continues_past_malformed_manifest(self) -> None:
        self.store.manifest_path("broken").write_text("{", encoding="utf-8")
        wt = self.spawn()
        rc, response = self.invoke("clean", "--all")
        self.assertEqual(rc, EXIT_LANE_ERROR)
        self.assertEqual(response["data"]["counts"]["blocked"], 1)
        self.assertEqual(response["data"]["counts"]["cleaned"], 1)
        self.assertFalse(wt.exists())
        self.assertEqual(self.store.manifest_path("broken").read_text(encoding="utf-8"), "{")

    def test_preview_does_not_initialize_an_unmanaged_repository(self) -> None:
        fresh = Path(self.temp.name) / "fresh"
        fresh.mkdir()
        git(fresh, "init", "-b", "main")
        store = LaneStore(fresh)
        self.assertEqual(store.clean_many(dry_run=True)["lanes"], [])
        self.assertEqual(store.clean("missing", dry_run=True)["status"], "blocked")
        self.assertFalse(store.root.exists())


    def test_bulk_cleanup_reports_partial_results_and_keeps_active_work(self) -> None:
        done = self.spawn()
        active = self.spawn("active")
        commit_file(active, "app.txt", "unfinished\n", "unfinished")
        self.spawn("history")
        self.store.clean("history", delete_branch=True)
        manifest_before = self.store.manifest_path("done").read_bytes()
        rc, preview = self.invoke("clean", "--all", "--dry-run", "--delete-branch")
        self.assertEqual(rc, EXIT_LANE_ERROR)
        self.assertTrue(preview["ok"])
        self.assertEqual(preview["data"]["counts"]["ready"], 1)
        self.assertEqual(preview["data"]["counts"]["blocked"], 1)
        self.assertEqual(self.store.manifest_path("done").read_bytes(), manifest_before)
        rc, response = self.invoke("clean", "--all", "--delete-branch")
        self.assertEqual(rc, EXIT_LANE_ERROR)
        self.assertEqual(response["data"]["counts"], {
            "ready": 0, "cleaned": 1, "already-cleaned": 1, "blocked": 1,
        })
        self.assertFalse(done.exists())
        self.assertTrue(active.exists())

    def test_cli_requires_an_explicit_cleanup_selection(self) -> None:
        for args in (("clean",), ("clean", "done", "--all")):
            rc, response = self.invoke(*args)
            self.assertEqual(rc, EXIT_USAGE)
            self.assertFalse(response["ok"])
        rc, response = self.invoke("clean", "--all", "--dry-run")
        self.assertEqual(rc, EXIT_OK)
        self.assertEqual(response["data"]["lanes"], [])

    def test_junction_or_symlink_is_not_traversed_or_removed(self) -> None:
        wt = self.spawn()
        outside = Path(self.temp.name) / "outside"
        outside.mkdir()
        sentinel = outside / "keep.txt"
        sentinel.write_text("do not delete", encoding="utf-8")
        link = wt / "linked"
        if os.name == "nt":
            link_arg = str(link).replace("'", "''")
            outside_arg = str(outside).replace("'", "''")
            command = subprocess.run(
                ["powershell", "-NoProfile", "-Command",
                 f"New-Item -ItemType Junction -Path '{link_arg}' -Target '{outside_arg}' | Out-Null"],
                capture_output=True, text=True,
            )
            if command.returncode:
                self.fail(command.stderr)
        else:
            link.symlink_to(outside, target_is_directory=True)
        try:
            self.assert_blocked("done", "symlink/junction")
            self.assertEqual(sentinel.read_text(encoding="utf-8"), "do not delete")
        finally:
            if os.name == "nt":
                link.rmdir()
            else:
                link.unlink()


if __name__ == "__main__":
    unittest.main()

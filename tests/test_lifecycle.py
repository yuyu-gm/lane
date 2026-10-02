"""Preserved v1 scenarios executed directly against the native CLI."""
from __future__ import annotations

import tempfile
import unittest
import json
import io
import os
import subprocess
import sys
from contextlib import redirect_stdout
from pathlib import Path

from support import EXIT_GIT_ERROR, EXIT_INVALID, EXIT_OK, cli_main
from support import LaneError, CliStore as LaneStore, git, git_path, resolve


def commit_file(repo: Path, rel: str, text: str, message: str) -> str:
    path = repo / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    git(repo, "add", rel)
    git(repo, "commit", "-m", message)
    return resolve(repo, "HEAD")


class V1LaneTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="lane-tests-")
        self.repo = Path(self.temp.name) / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-b", "main")
        git(self.repo, "config", "user.name", "Lane Tests")
        git(self.repo, "config", "user.email", "lane-tests@example.invalid")
        commit_file(self.repo, "app.txt", "alpha\nbeta\ngamma\n", "initial")
        self.store = LaneStore(self.repo)
        self.store.ensure()

    def tearDown(self) -> None:
        try:
            git(self.repo, "worktree", "prune", check=False)
        finally:
            self.temp.cleanup()

    def test_init_layout_is_inside_project_and_ignored(self) -> None:
        self.assertEqual(self.store.root, self.repo / ".agent-tools" / "lane")
        self.assertTrue((self.store.root / "worktrees").is_dir())
        self.assertEqual(git(self.repo, "status", "--porcelain").stdout.strip(), "")
        config = json.loads((self.store.root / "config.json").read_text(encoding="utf-8"))
        self.assertEqual(config["agent_execution"], "external")
        self.assertEqual(config["interface"], "agent-json")
        self.assertEqual(config["version"], 3)
        self.assertNotIn("runner", config)

    def test_spawn_preflight_integrate_and_cleanup(self) -> None:
        manifest = self.store.spawn("worker-a", expected_paths=["app.txt"])
        wt = Path(manifest["worktree"])
        self.assertTrue(wt.is_dir())
        self.assertTrue(str(wt).startswith(str(self.repo / ".agent-tools" / "lane" / "worktrees")))
        context = self.store.agent_context("worker-a")
        self.assertEqual(context["cwd"], str(wt))
        self.assertEqual(context["branch"], "lane/worker-a")
        self.assertEqual(context["state"], "empty")
        self.assertTrue(context["constraints"]["worktree_only"])
        self.assertFalse(context["constraints"]["branch_switch"])

        commit_file(wt, "app.txt", "alpha\nbeta lane\ngamma\n", "lane change")
        validation = self.store.validate(self.store.load("worker-a"))
        self.assertTrue(validation["valid"])
        self.assertEqual(validation["changed_paths"], ["app.txt"])

        preflight = self.store.preflight("worker-a")
        self.assertEqual(preflight["status"], "clean")
        receipt = self.store.integrate("worker-a")
        self.assertEqual(receipt["status"], "integrated")
        self.assertIn("beta lane", (self.repo / "app.txt").read_text(encoding="utf-8"))

        self.store.clean("worker-a", delete_branch=True)
        self.assertFalse(wt.exists())
        cleaned_status = self.store.lane_status("worker-a")
        self.assertTrue(cleaned_status["valid"])
        self.assertTrue(cleaned_status["integrated"])
        self.assertEqual(cleaned_status["state"], "cleaned")
        self.assertNotEqual(
            git(self.repo, "show-ref", "--verify", "--quiet", "refs/heads/lane/worker-a", check=False).returncode,
            0,
        )

    def test_unexpected_path_is_rejected(self) -> None:
        manifest = self.store.spawn("scope", expected_paths=["app.txt"])
        wt = Path(manifest["worktree"])
        commit_file(wt, "other.txt", "outside\n", "outside scope")
        validation = self.store.validate(self.store.load("scope"))
        self.assertFalse(validation["valid"])
        self.assertEqual(validation["unexpected_paths"], ["other.txt"])
        self.assertEqual(self.store.preflight("scope")["status"], "invalid")

    def test_dot_prefixed_paths_preserve_leading_dot(self) -> None:
        manifest = self.store.spawn("dot-scope", expected_paths=[".github"])
        wt = Path(manifest["worktree"])
        commit_file(wt, ".github/workflows/ci.yml", "name: ci\n", "workflow")
        validation = self.store.validate(self.store.load("dot-scope"))
        self.assertTrue(validation["valid"])
        self.assertEqual(validation["changed_paths"], [".github/workflows/ci.yml"])

    def test_changed_paths_handles_non_ascii_and_spaces_with_quotepath(self) -> None:
        git(self.repo, "config", "core.quotePath", "true")
        manifest = self.store.spawn("unicode", expected_paths=["docs"])
        wt = Path(manifest["worktree"])
        rel = "docs/日本語 file.md"
        commit_file(wt, rel, "ok\n", "unicode path")
        validation = self.store.validate(self.store.load("unicode"))
        self.assertTrue(validation["valid"])
        self.assertEqual(validation["changed_paths"], [rel])

    def test_uncommitted_worker_changes_are_invalid(self) -> None:
        manifest = self.store.spawn("dirty", expected_paths=["app.txt"])
        wt = Path(manifest["worktree"])
        (wt / "app.txt").write_text("uncommitted\n", encoding="utf-8")
        validation = self.store.validate(self.store.load("dirty"))
        self.assertFalse(validation["valid"])
        self.assertTrue(validation["worktree_dirty"])
        self.assertIn("lane worktree has uncommitted changes", validation["errors"])
        self.assertEqual(self.store.preflight("dirty")["status"], "invalid")

    def test_conflict_packet_is_written(self) -> None:
        manifest = self.store.spawn("conflict", expected_paths=["app.txt"])
        wt = Path(manifest["worktree"])
        commit_file(wt, "app.txt", "alpha\nworker\ngamma\n", "worker edit")
        commit_file(self.repo, "app.txt", "alpha\nparent\ngamma\n", "parent edit")

        preflight = self.store.preflight("conflict")
        self.assertEqual(preflight["status"], "conflict")
        self.assertIn("app.txt", preflight["conflict_paths"])
        packets = list((self.store.conflicts).glob("*-conflict.json"))
        self.assertTrue(packets)

    def test_sequential_plan_detects_cross_lane_conflict(self) -> None:
        first = self.store.spawn("first", expected_paths=["app.txt"])
        second = self.store.spawn("second", expected_paths=["app.txt"])
        commit_file(Path(first["worktree"]), "app.txt", "alpha\nFIRST\ngamma\n", "first")
        commit_file(Path(second["worktree"]), "app.txt", "alpha\nSECOND\ngamma\n", "second")

        plan = self.store.plan(["first", "second"])
        self.assertEqual(plan["status"], "blocked")
        self.assertEqual(plan["steps"][0]["status"], "clean")
        self.assertEqual(plan["steps"][1]["status"], "conflict")

    def test_cleanup_refuses_unintegrated_lane(self) -> None:
        manifest = self.store.spawn("keep", expected_paths=["app.txt"])
        commit_file(Path(manifest["worktree"]), "app.txt", "changed\n", "change")
        with self.assertRaises(LaneError):
            self.store.clean("keep")

    def test_validation_uses_branch_tip_when_a_tag_has_the_same_name(self) -> None:
        manifest = self.store.spawn("same-name", expected_paths=["app.txt"])
        git(self.repo, "tag", "lane/same-name")
        tip = commit_file(Path(manifest["worktree"]), "app.txt", "worker change\n", "worker")
        check = self.store.preflight("same-name")
        self.assertEqual(check["branch_commit"], tip)
        self.assertEqual(check["status"], "clean")
        receipt = self.store.integrate("same-name")
        self.assertEqual(receipt["branch_commit"], tip)
        self.assertEqual((self.repo / "app.txt").read_text(encoding="utf-8"), "worker change\n")


    def test_default_bulk_operations_skip_cleaned_lane_history(self) -> None:
        old = self.store.spawn("old", expected_paths=["app.txt"])
        commit_file(Path(old["worktree"]), "app.txt", "old\n", "old change")
        self.store.integrate("old")
        self.store.clean("old", delete_branch=True)

        fresh = self.store.spawn("fresh", expected_paths=["fresh.txt"])
        commit_file(Path(fresh["worktree"]), "fresh.txt", "fresh\n", "fresh change")

        self.assertEqual(self.store.list_ids(), ["fresh", "old"])
        self.assertEqual([row["id"] for row in self.store.invoke("status", "--unintegrated")["lanes"]], ["fresh"])
        plan = self.store.plan([])
        self.assertEqual(plan["status"], "clean")
        self.assertEqual(plan["lanes"], ["fresh"])
        result = self.store.integrate_many([])
        self.assertEqual(result["status"], "integrated")
        self.assertEqual([receipt["lane"] for receipt in result["receipts"]], ["fresh"])

    def test_empty_commit_is_integrated_and_can_be_cleaned(self) -> None:
        manifest = self.store.spawn("empty-commit", expected_paths=["app.txt"])
        wt = Path(manifest["worktree"])
        git(wt, "commit", "--allow-empty", "-m", "empty commit")
        tip = resolve(wt, "HEAD")

        validation = self.store.validate(self.store.load("empty-commit"))
        self.assertEqual(validation["commit_count"], 1)
        self.assertFalse(validation["has_changes"])
        self.assertEqual(self.store.preflight("empty-commit")["status"], "clean")
        self.assertEqual(self.store.plan(["empty-commit"])["status"], "clean")
        receipt = self.store.integrate("empty-commit")
        self.assertEqual(receipt["status"], "integrated")
        self.assertEqual(receipt["branch_commit"], tip)
        self.assertEqual(self.store.lane_status("empty-commit")["state"], "integrated")
        self.store.clean("empty-commit", delete_branch=True)

    def test_reverted_net_diff_history_is_integrated(self) -> None:
        manifest = self.store.spawn("reverted", expected_paths=["app.txt"])
        wt = Path(manifest["worktree"])
        original = (wt / "app.txt").read_text(encoding="utf-8")
        commit_file(wt, "app.txt", "temporary\n", "temporary change")
        commit_file(wt, "app.txt", original, "revert change")

        validation = self.store.validate(self.store.load("reverted"))
        self.assertEqual(validation["commit_count"], 2)
        self.assertFalse(validation["has_changes"])
        self.assertEqual(self.store.preflight("reverted")["status"], "clean")
        receipt = self.store.integrate("reverted")
        self.assertEqual(receipt["status"], "integrated")
        self.assertEqual((self.repo / "app.txt").read_text(encoding="utf-8"), original)
        self.store.clean("reverted", delete_branch=True)

    def test_integrated_lane_becomes_active_after_new_commit(self) -> None:
        manifest = self.store.spawn("continued", expected_paths=["app.txt"])
        wt = Path(manifest["worktree"])
        commit_file(wt, "app.txt", "first\n", "first")
        self.store.integrate("continued")
        self.assertEqual(self.store.lane_status("continued")["state"], "integrated")

        commit_file(wt, "app.txt", "second\n", "second")
        status = self.store.lane_status("continued")
        self.assertFalse(status["integrated"])
        self.assertEqual(status["state"], "active")
        self.assertEqual(self.store.agent_context("continued")["state"], "active")

        self.store.integrate("continued")
        status = self.store.lane_status("continued")
        self.assertTrue(status["integrated"])
        self.assertEqual(status["state"], "integrated")


    def test_replay_starts_from_current_parent_and_preserves_source_lane(self) -> None:
        manifest = self.store.spawn("source", expected_paths=["app.txt"])
        source_wt = Path(manifest["worktree"])
        commit_file(source_wt, "app.txt", "alpha\nsource\ngamma\n", "source edit")
        parent_tip = commit_file(self.repo, "app.txt", "alpha\nparent\ngamma\n", "parent edit")

        replay = self.store.replay("source", new_id="source-replay")
        self.assertEqual(replay["base_commit"], parent_tip)
        self.assertEqual(replay["replay_of"], "source")
        self.assertTrue(source_wt.exists())
        self.assertEqual(replay["source_branch"], "lane/source")
        self.assertEqual(replay["source_branch_commit"], resolve(self.repo, "lane/source"))
        replay_context = json.loads(Path(replay["replay_context"]).read_text(encoding="utf-8"))
        self.assertIn("source", replay_context["source_diff"])

    def test_cli_is_single_line_json_protocol(self) -> None:
        output = io.StringIO()
        with redirect_stdout(output):
            rc = cli_main(["--project", str(self.repo), "spawn", "machine", "--expected", "app.txt"])
        self.assertEqual(rc, EXIT_OK)
        lines = output.getvalue().splitlines()
        self.assertEqual(len(lines), 1)
        response = json.loads(lines[0])
        self.assertTrue(response["ok"])
        self.assertEqual(response["command"], "spawn")
        self.assertEqual(response["data"]["cwd"], str(self.store.worktrees / "machine"))
        self.assertNotIn("runner", response["data"])


    def test_cli_validation_failure_has_stable_exit_code(self) -> None:
        manifest = self.store.spawn("cli-invalid", expected_paths=["app.txt"])
        commit_file(Path(manifest["worktree"]), "outside.txt", "bad\n", "outside")
        output = io.StringIO()
        with redirect_stdout(output):
            rc = cli_main(["--project", str(self.repo), "validate", "cli-invalid"])
        self.assertEqual(rc, EXIT_INVALID)
        response = json.loads(output.getvalue())
        self.assertTrue(response["ok"])
        self.assertFalse(response["data"]["valid"])


if __name__ == "__main__":
    unittest.main()

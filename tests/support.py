"""Test-only Git helpers, fixture access and a subprocess adapter for the CLI.

No validation, integration or cleanup implementation lives here. Every Lane
operation runs the native executable; fixture access supports corruption tests.
"""

from contextlib import contextmanager
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
EXE = Path(os.environ.get("LANE_NATIVE_EXE", ROOT / "target/release" /
                      ("lane.exe" if os.name == "nt" else "lane")))
EXIT_OK, EXIT_USAGE, EXIT_INVALID, EXIT_LANE_ERROR, EXIT_GIT_ERROR = 0, 2, 10, 12, 13


class LaneError(RuntimeError):
    pass


class GitError(LaneError):
    pass


def git(repo, *args, check=True):
    result = subprocess.run(["git", "-C", str(repo), *map(str, args)],
                            capture_output=True, text=True, encoding="utf-8")
    if check and result.returncode:
        raise GitError(f"git {args}: {result.stderr}")
    return result


def resolve(repo, ref):
    return git(repo, "rev-parse", "--verify", ref).stdout.strip()


def git_path(repo, name):
    path = Path(git(repo, "rev-parse", "--git-path", name).stdout.strip())
    return path if path.is_absolute() else Path(repo) / path


@contextmanager
def fixture_lock(path):
    """Hold the documented byte lock to test contention from another process."""
    with path.open("a+b") as handle:
        handle.seek(0, os.SEEK_END)
        if not handle.tell():
            handle.write(b"\0")
            handle.flush()
        handle.seek(0)
        if os.name == "nt":
            import msvcrt
            msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
        else:
            import fcntl
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        try:
            yield
        finally:
            handle.seek(0)
            if os.name == "nt":
                msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def cli_main(argv):
    result = subprocess.run([str(EXE), *map(str, argv)], capture_output=True)
    print(result.stdout.decode("ascii").strip())
    return result.returncode


class CliStore:
    """Translate old scenario calls to the public CLI; read/write fixture JSON."""
    def __init__(self, repo):
        self.repo = Path(repo)
        self.root = self.repo / ".agent-tools/lane"
        self.worktrees = self.root / "worktrees"
        self.manifests = self.root / "manifests"
        self.receipts = self.root / "receipts"
        self.conflicts = self.root / "conflicts"

    def manifest_path(self, identifier):
        return self.manifests / f"{identifier}.json"

    def load(self, identifier):
        return json.loads(self.manifest_path(identifier).read_text(encoding="utf-8"))

    def list_ids(self):
        return sorted(path.stem for path in self.manifests.glob("*.json"))

    def save(self, manifest):
        self.manifest_path(manifest["id"]).write_text(json.dumps(manifest) + "\n", encoding="utf-8")

    def invoke(self, *args):
        result = subprocess.run([str(EXE), "--project", str(self.repo), *map(str, args)], capture_output=True)
        envelope = json.loads(result.stdout)
        if not envelope["ok"]:
            error = envelope["error"]
            raise (GitError if error["code"] == "git_error" else LaneError)(error["message"])
        return envelope["data"]

    def ensure(self):
        self.invoke("init")

    def integration_lock(self):
        return fixture_lock(self.root / "integration.lock")

    def spawn(self, identifier, *, base="HEAD", branch=None, expected_paths=()):
        args = ["spawn", identifier, "--base", base]
        if branch:
            args += ["--branch", branch]
        for path in expected_paths:
            args += ["--expected", path]
        self.invoke(*args)
        return self.load(identifier)

    def agent_context(self, identifier):
        return self.invoke("context", identifier)

    def lane_status(self, identifier):
        return self.invoke("status", identifier)["lanes"][0]

    def validate(self, manifest):
        return self.invoke("validate", manifest["id"])

    def preflight(self, identifier, *, target="HEAD"):
        return self.invoke("preflight", identifier, "--target", target)

    def plan(self, identifiers, *, target="HEAD"):
        return self.invoke("plan", *identifiers, "--target", target)

    def integrate(self, identifier):
        return self.invoke("integrate", identifier)

    def integrate_many(self, identifiers):
        return self.invoke("integrate-all", *identifiers)

    def clean(self, identifier, *, delete_branch=False, dry_run=False):
        args = ["clean", identifier]
        if delete_branch:
            args += ["--delete-branch"]
        if dry_run:
            args += ["--dry-run"]
        result = self.invoke(*args)
        return result if dry_run else self.load(identifier)

    def cleanup_check(self, identifier, *, delete_branch=False):
        return self.clean(identifier, delete_branch=delete_branch, dry_run=True)

    def clean_many(self, *, delete_branch=False, dry_run=False):
        args = ["clean", "--all"]
        if delete_branch:
            args += ["--delete-branch"]
        if dry_run:
            args += ["--dry-run"]
        return self.invoke(*args)

    def replay(self, identifier, *, new_id=None, target="HEAD"):
        args = ["replay", identifier, "--target", target]
        if new_id:
            args += ["--new-id", new_id]
        result = self.invoke(*args)
        return self.load(result["id"])

"""Demonstrate the real native CLI in disposable repositories; no agents needed."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def git(repo, *args):
    result = subprocess.run(
        ["git", "-C", str(repo), *args], capture_output=True, text=True,
        encoding="utf-8", timeout=30,
    )
    require(result.returncode == 0, f"git {args}: {result.stderr}")
    return result.stdout.strip()


def lane(executable, repo, *args, code=0):
    result = subprocess.run(
        [str(executable), "--project", str(repo), *map(str, args)],
        capture_output=True, timeout=30,
    )
    require(result.returncode == code, f"lane {args}: {result.stdout!r} {result.stderr!r}")
    require(len(result.stdout.splitlines()) == 1, "Expected one JSON response line")
    envelope = json.loads(result.stdout)
    return envelope["data"] if envelope["ok"] else envelope["error"]


def commit(repo, path, content):
    destination = repo / path
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(content, encoding="utf-8")
    git(repo, "add", "--", path)
    git(repo, "commit", "-m", f"demo: {path}")


def repository(path):
    path.mkdir()
    git(path, "init", "-b", "main")
    git(path, "config", "user.name", "Lane Demo")
    git(path, "config", "user.email", "lane-demo@local.invalid")
    git(path, "config", "commit.gpgsign", "false")
    commit(path, "app.txt", "alpha\nbeta\ngamma\n")
    return path


def snapshot(repo):
    index = Path(git(repo, "rev-parse", "--git-path", "index"))
    if not index.is_absolute():
        index = repo / index
    return git(repo, "rev-parse", "HEAD"), git(repo, "show-ref"), index.read_bytes()


def sequential_conflict(executable, root):
    repo = repository(root / "conflicts")
    a = Path(lane(executable, repo, "spawn", "a", "--expected", "app.txt")["cwd"])
    b = Path(lane(executable, repo, "spawn", "b", "--expected", "app.txt")["cwd"])
    commit(a, "app.txt", "alpha\nchange-a\ngamma\n")
    commit(b, "app.txt", "alpha\nchange-b\ngamma\n")
    before = snapshot(repo)
    for identifier in ("a", "b"):
        require(lane(executable, repo, "preflight", identifier)["status"] == "clean",
                "Each lane should merge clean against the original parent")
        print(f"preflight {identifier}: clean")
    plan = lane(executable, repo, "plan", "a", "b", code=11)
    require(plan["status"] == "blocked" and plan["steps"][1]["status"] == "conflict",
            "Expected conflict at the second step")
    blocked = lane(executable, repo, "integrate-all", "a", "b", code=11)
    require(blocked["receipts"] == [], "Blocked plan must not start integration")
    require(snapshot(repo) == before, "Parent HEAD, refs or index changed")
    print("plan a b: blocked at b (exit 11)")
    print("integrate-all a b: blocked; parent HEAD, refs and index unchanged")


def artifact_cleanup(executable, root):
    repo = repository(root / "artifacts")
    commit(repo, ".gitignore", "out/\n")
    worker = Path(lane(executable, repo, "spawn", "build", "--expected", "app.txt")["cwd"])
    commit(worker, "app.txt", "alpha\ncompleted\ngamma\n")
    artifact = worker / "out/build.log"
    artifact.parent.mkdir()
    payload = b"demo build output\n"
    artifact.write_bytes(payload)
    require(lane(executable, repo, "validate", "build")["valid"], "Worker validation failed")
    lane(executable, repo, "integrate", "build")
    refused = lane(executable, repo, "clean", "build", "--delete-branch", code=12)
    require(refused["code"] == "lane_error" and artifact.read_bytes() == payload,
            "Cleanup must preserve the uncollected artifact")
    print("clean build: refused; ignored out/build.log preserved (exit 12)")
    collection = lane(executable, repo, "collect", "build", "--dest", root / "archive")
    require(collection["status"] == "verified", "Collection did not verify")
    archived = Path(collection["files"][0]["destination"])
    require(archived.read_bytes() == payload, "Archived content differs")
    print("collect build: verified; source and archive SHA-256 match")
    receipt = collection["receipt"]
    preview = lane(executable, repo, "clean", "build", "--dry-run", "--collection", receipt,
                   "--delete-branch")
    require(preview["status"] == "ready", "Cleanup preview did not pass")
    lane(executable, repo, "clean", "build", "--collection", receipt, "--delete-branch")
    require(not worker.exists(), "Worker worktree remains")
    require(hashlib.sha256(archived.read_bytes()).digest() == hashlib.sha256(payload).digest(),
            "Cleanup changed the archive")
    print("clean build --collection: completed; verified archive preserved")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lane", required=True, type=Path,
                        help="Explicit path to a built native executable; never resolved via PATH")
    args = parser.parse_args()
    executable = args.lane.resolve(strict=True)
    # TemporaryDirectory owns only the fresh demo directory, never the checkout.
    with tempfile.TemporaryDirectory(prefix="lane-demo-") as temporary:
        root = Path(temporary).resolve()
        schema = lane(executable, root, "schema")
        require(schema.get("implementation") == "rust" and "collect" in schema["commands"],
                "Use the native CLI with artifact collection support")
        sequential_conflict(executable, root)
        artifact_cleanup(executable, root)
    print("Demo passed; temporary repositories removed. Installed Lane and global settings untouched.")


if __name__ == "__main__":
    main()

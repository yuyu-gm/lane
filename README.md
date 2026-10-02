# Lane

[日本語](README.ja.md)

Lane manages Git worktrees for parallel coding agents. It gives each worker a branch, checks the changed paths and merge conflicts, and merges finished work back into the parent branch. Workers are started through your own agent tools.

## Build

You need Git 2.38+ and Rust 1.90+. On Windows, use the MSVC toolchain with the C++ build tools.

```powershell
cargo build --release --locked
./target/release/lane.exe schema
```

Windows x64 is tested. Linux and macOS have not been verified. The executable needs Git at runtime; Python is needed for the tests and demo.

To build a Windows bundle and run its checks, with Python 3.11+ installed:

```powershell
powershell -NoProfile -File scripts/build.ps1
```

The bundle is written to `dist/lane-<version>/`. Run its executable directly, or follow the [installation instructions](docs/installation.md). Installation places `LANE.md` beside the agent configuration's `AGENTS.md` and adds a reference while keeping the existing instructions.

## Usage

Run these commands in the Git repository you want to work on. Use the built executable's path if `lane` is not on PATH.

```text
lane init
lane spawn api --expected src/api --expected tests/api
```

Give the worker the returned `cwd`, `branch`, `base_commit` and constraints. Have it commit the finished work, then stop writing. Review the diff and run the project's checks before merging:

```text
lane validate api
lane preflight api
lane integrate api
lane clean api --dry-run --delete-branch
lane clean api --delete-branch
```

Each command prints JSON. Check its exit status and result before proceeding; `lane schema` lists the commands, arguments and exit codes.

For multiple completed lanes, check the intended merge order:

```text
lane plan api ui
lane integrate-all api ui
```

`plan` checks each merge against the preceding result, so it can catch conflicts between workers that pass their individual preflights. It leaves the checkout, index and branch refs unchanged.

Ignored build outputs also block cleanup. Use `collect` to archive them, then pass the returned receipt to `clean --collection`. The demo shows this and a conflict between two lanes:

```powershell
python -X utf8 examples/demo.py --lane ./target/release/lane.exe
```

The complete workflow and JSON protocol are in [docs/LANE.md](docs/LANE.md).

## Limits

Stop workers and other writers during integration, collection and cleanup. Worktrees share Git objects and refs; they do not isolate permissions. A clean Git merge still needs code review and tests.

Batch integration is sequential. If a later merge fails, earlier merges may remain applied; inspect the receipts and current state before retrying.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for checks, packaging and the source layout.

## License

[MIT](LICENSE).

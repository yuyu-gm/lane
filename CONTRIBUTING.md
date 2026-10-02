# Contributing

Use Rust 1.90+, Git 2.38+ and Python 3.11+. The Python scripts use the standard library and run the built CLI in temporary repositories.

## Checks

From the repository root:

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
python -X utf8 -m unittest discover -s tests -v
python -X utf8 examples/demo.py --lane ./target/release/lane.exe
git diff --check
```

Git conflict, corruption and deletion tests must use temporary repositories. Keep the built executable's JSON output and exit codes compatible with [the protocol](docs/LANE.md). Update that guide when behavior changes.

## Windows packaging

```powershell
powershell -NoProfile -File scripts/build.ps1
```

This runs the Rust checks, executable tests and installation checks, then leaves the bundle in `dist/lane-<version>/`. Installation tests select temporary destinations. `-SkipTests` is for repackaging a checkout whose checks have already passed.

The Windows CI job runs the same build and the demo. Linux/macOS executable tests are welcome; those platforms are not yet verified.

## Layout

- `src/`: CLI, Git lifecycle, artifact collection and cleanup.
- `tests/`: executable scenarios and installation checks; `fixtures/` holds the Git shim and scope cases.
- `scripts/`: Windows build/install scripts and the startup benchmark.
- `examples/`: runnable demo.
- `docs/LANE.md`: standalone agent guide, copied unchanged into the bundle.

The benchmark can be run with `python scripts/benchmark.py --output target/benchmark.json`. Test helpers invoke the CLI; they do not contain another Lane implementation.

## Releases

Check the bundle's `SHA256SUMS.json`, version and guide/license copies before publishing. Include the source revision and supported platform in release notes. Publish the bundle payload, not local logs or backups.

Keep generated `target/` and `dist/`, operational state and local settings out of commits. The root `AGENTS.md` is a maintainer's local file; installation creates a separate reference block in the destination's own instructions.

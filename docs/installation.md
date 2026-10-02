# Windows installation

Build a bundle from the checkout with `powershell -NoProfile -File scripts/build.ps1`, then open `dist/lane-<version>/`. You can use `lane.exe` directly from that directory.

## Install

In the bundle directory, preview the destination paths:

```powershell
powershell -NoProfile -File install.ps1
```

To install:

```powershell
powershell -NoProfile -File install.ps1 -Apply
```

The script verifies the bundle's hashes and updates:

- `%LOCALAPPDATA%\Programs\lane\lane.exe`
- `%USERPROFILE%\.codex\LANE.md`
- `%USERPROFILE%\.codex\AGENTS.md`: a reference block pointing to the colocated guide.

Use `-InstallDir` and `-GuideDir` to select other destinations:

```powershell
powershell -NoProfile -File install.ps1 -InstallDir C:/tools/lane -GuideDir C:/path/to/agent-config
```

Add `-Apply` after checking the preview. Preview writes no files. The script leaves PATH unchanged; use the EXE's path or an existing PATH entry. Runtime requires Git 2.38+.

## Agent instructions

The guide is copied unchanged as `LANE.md`. The installer adds or updates this block in the destination's `AGENTS.md`:

```markdown
<!-- lane-managed-reference:start -->
## Lane

Before using Lane for isolated worker work, read and follow [LANE.md](LANE.md) in this same directory.
Lane does not require delegation for ordinary single-agent tasks.
<!-- lane-managed-reference:end -->
```

Other instructions are kept. Reinstalling updates the block without duplicating it. The checkout's local development `AGENTS.md` is never copied.

Existing instruction files must be UTF-8, with or without a BOM. Their line endings and surrounding text are preserved. Invalid encoding or unmatched/duplicate markers stop installation before any replacement.

## Recovery

Stop processes that use or write the destination files before installation. Existing files are backed up under the bundle's `backups/` directory. On failure, completed replacements are restored; the error reports any rollback that needs inspection.

Replacement is atomic per file, not across all three files. If the process is forcibly terminated, inspect the destinations and backups before retrying. Installation does not remove worktrees or archived artifacts.

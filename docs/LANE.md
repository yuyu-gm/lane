# Lane: Parent-Agent Usage Protocol

## Purpose and Ownership

Lane provisions Git branches and worktrees, validates completed changes, simulates merges, integrates work, and removes integrated worktrees. It does not start agents or choose their models, prompts, or retry policy.

The parent owns `spawn → external worker → validate → preflight/plan → integrate → verify → clean`. Use this protocol when the task calls for isolated worker work; its presence does not require delegation for ordinary single-agent tasks.

Workers edit only their assigned worktree and commit only to the assigned branch. They must not switch/create branches, rebase, merge the parent, push, or manage other lanes. The parent invokes workers through the available agent mechanism after provisioning their lanes.

Worktrees isolate checkouts, not permissions. Workers share Git objects, refs, and repository configuration. Returned constraints are a cooperative work contract, not an OS sandbox. Git merge checks establish mergeability; code review and functional tests remain the parent's responsibility.

## Discover the Interface

For agent installation, place this guide as `LANE.md` beside the agent configuration's `AGENTS.md`, and have that file reference `[LANE.md](LANE.md)` in the same directory. The installer adds or updates a managed reference block while preserving other instructions. Repository development instructions are local-only and must not replace an agent's configuration. Preparing a bundle alone does not authorize installation.

Before relying on remembered commands or fields, run:

```text
lane schema
lane --project <repo> doctor
```

`schema` reports the installed version, command names, protocol, and exit codes. The Python 0.3 interface does not contain a complete argument schema. The Rust 0.4 interface also supplies `argument_schema` (positionals, types, defaults, repeatability, choices, required arguments and selection constraints) and `contract_schema`. Global arguments go before the command. Do not infer installed behavior from another version or a source checkout. `doctor` reports Git and repository detection; success is not a substitute for validation and preflight. Git must support `merge-tree --write-tree`.

Command results use a single-line JSON envelope:

```json
{"format":"lane-agent-response/v1","ok":true,"command":"validate","data":{"valid":true}}
```

```json
{"format":"lane-agent-response/v1","ok":false,"command":"integrate","error":{"code":"blocked","message":"..."}}
```

Read both the process exit code and result fields. A completed check can have `ok:true` while `data.valid` is false or `data.status` reports a conflict. Never interpret `ok` alone as admission to integrate. Missing or malformed output requires inspecting state before retrying.

| Exit | Meaning | Next action |
|---:|---|---|
| 0 | Command completed successfully | Inspect the result before proceeding |
| 2 | Usage error | Correct the invocation |
| 10 | Invalid lane, scope, or unfinished work | Return work to the worker or investigate state |
| 11 | Conflict or integration blocked | Inspect artifacts and resolve |
| 12 | Lane operation error | Correct the reported orchestration problem |
| 13 | Git operation error | Inspect Git state and the JSON error |
| 130 | Interrupted | Re-read state; do not assume rollback |

## 1. Establish the Parent Checkout

Use an existing Git repository with an initial commit. Choose the intended parent branch and commit the base workers should receive: uncommitted parent changes are not part of a spawned worktree. Integration requires a clean, attached parent checkout.

```text
lane --project <repo> init
```

This creates Lane metadata, not a Git repository. Operational state lives under `<repo>/.agent-tools/lane/`; worker checkouts are under its `worktrees/` directory. Lane excludes its operational directory through Git's local exclude file.

## 2. Provision and Dispatch Work

```text
lane --project <repo> spawn parser --expected src/parser --expected tests/parser
```

Choose a unique task-oriented ID. Use narrow repository-relative expected paths; an empty list allows unrestricted changed paths. `spawn` records a pinned `base_commit` and returns a `lane-agent-context/v1` object containing:

| Field | Worker handoff |
|---|---|
| `cwd` | Assigned working directory |
| `branch` | Only branch the worker may commit to |
| `base_commit` | Recorded starting commit |
| `expected_paths` | Allowed edit scope |
| `constraints` | Worktree and Git restrictions |

Pass these returned values with the objective and acceptance criteria. Do not reconstruct paths or branch names. Recover the context with `lane --project <repo> context parser`.

Include this completion contract in the worker instruction:

```text
Work only in the assigned cwd and stay within expected_paths.
Follow the supplied constraints; do not switch/create branches, rebase,
merge the parent, push, or operate another lane.
Run the relevant checks and commit completed work to the assigned branch.
Inspect git status and git diff before returning.
Report the final commit ID and checks run, then stop writing until reassigned.
```

## 3. Validate and Review Completed Work

Wait for workers and their background Git processes to stop writing. Keep them idle through integration and verification. Worker completion messages alone are not proof of completion.

```text
lane --project <repo> validate parser
```

Require successful validation. It checks ancestry from the pinned base, changed-path scope, assigned checkout branch, and uncommitted work. Review the actual change and run task-appropriate checks. Validation of a final tree difference does not audit every intermediate commit or worker action.

Lane resolves the assigned branch through its full `refs/heads/` name so a same-named tag cannot substitute another commit. A failed worker `git status` check is a Git operation error (exit 13), not evidence of a clean checkout.

Record the validated `branch_commit` and parent HEAD. Investigate unexpected missing worktrees or warnings. Return unfinished or out-of-scope work for correction before proceeding.

## 4. Preflight the Intended Integration

Use `preflight` for one lane or `plan` for an ordered sequence:

```text
lane --project <repo> preflight parser
lane --project <repo> plan parser api tests
```

`preflight` uses `git merge-tree` without changing the checkout, index, or branch. `plan` creates synthetic commits in Git's object database and uses each virtual result as the next merge target. This detects conflicts between lanes that individually preflight clean against the same parent.

Pass explicit lane IDs in the intended order. A change to a worker tip, or a parent HEAD change outside that sequence, requires fresh validation and planning.

| Result | Interpretation |
|---|---|
| `clean` | Proceed to integration after review |
| `already-integrated` | Verify ancestry before treating the lane as complete |
| `empty` | Inspect commit count and ancestry to determine whether work remains |
| `invalid` | Correct lane state or scope |
| `conflict` / blocked plan | Read artifacts and use resolution flow |

## 5. Integrate and Verify

Use the single-lane or multi-lane command as appropriate:

```text
lane --project <repo> integrate parser
lane --project <repo> integrate-all parser api tests
```

`integrate-all` first plans the sequence, then merges each lane. Integration rechecks admission and uses an integration lock. That lock coordinates Lane integration calls, not workers or unrelated Git commands; keep exclusive write ownership of the parent checkout during integration.

Verify that each recorded worker commit is an ancestor of parent HEAD, the worker branch still has the reviewed tip, and the receipts describe those commits. Run the relevant checks on the combined parent result before cleanup.

Integration is sequential, not an all-or-nothing transaction. A later failure may leave earlier lanes integrated. Inspect current HEAD, worktree status, and receipts; preserve completed work and replan only the remaining lanes. Do not automatically reset successful integrations or blindly retry the entire batch.

## 6. Resolve Conflicts While Preserving Work

1. Read the returned preflight or plan artifact and any conflict packet.
2. Preserve the original lane branch and its intended change.
3. Create a resolution lane with `lane --project <repo> replay parser`.
4. Give a resolution worker the returned context, source metadata, artifacts, and acceptance criteria for preserving both behaviors.
5. Have it commit the resolution, then validate, review, and preflight normally.

A replay starts from the real parent HEAD; it does not automatically apply the source diff or start from a plan's synthetic result. For a sequential conflict, include preceding lane context and replan the remaining intended sequence after resolution. Do not use one-sided merge strategies that discard either side's intent.

## 7. Cleanup and Follow-Up

Cleanup is mandatory for every used lane, including replay lanes. After integration and verification, the parent MUST run
`lane clean` for each completed lane before reporting orchestration complete. Do not leave eligible worktrees behind for
unspecified future cleanup. Verify the command result and resulting lane/worktree state, and include cleanup outcomes in
the final handoff. If safety checks block cleanup, preserve the work, report the lane ID, exact reason, and next action,
and keep cleanup explicitly outstanding until it succeeds; never force removal to satisfy this requirement.

```text
lane --project <repo> clean parser --delete-branch
```

Stop workers and background processes before cleanup and keep them idle until it finishes. Clean only after successful integration and verification. The parent must be on an attached branch; cleanup checks the actual lane branch tip against current parent HEAD, including for lanes previously marked integrated. An unused lane with no new commits is also eligible if its base is included in HEAD.

Preview one lane or all recorded lanes before removal:

```text
lane --project <repo> clean parser --dry-run --delete-branch
lane --project <repo> clean --all --dry-run --delete-branch
lane --project <repo> clean --all --delete-branch
```

Exactly one of an ID and `--all` is required. Without `--delete-branch`, cleanup retains the branch. `--dry-run` performs read-only eligibility checks: it does not remove worktrees/branches, write manifests, or initialize Lane state. Execution repeats the checks under the same lock used by integration. The lock coordinates Lane integration/cleanup calls, not workers, spawn/replay calls, or other Git commands; exclusive external write ownership remains necessary.

Cleanup refuses:

- A branch tip not included in current HEAD, or a missing branch without a completed deletion record.
- Uncommitted, untracked, or ignored files, including ignored build outputs and local settings. Inspect and explicitly preserve or remove those files yourself before retrying.
- A detached/switched worker checkout, a lane branch checked out elsewhere, a locked/prunable worktree, or unfinished merge/rebase/cherry-pick/revert/bisect state.
- A manifest path outside its assigned location, a foreign/unregistered worktree, a missing but still registered worktree, or symlinks/junctions in the managed path or worktree.
- A worktree or deleted branch that reappeared after cleanup, or a retained branch that differs from its recorded cleanup tip. Older manifests without a recorded tip can require manual inspection.

Rust 0.4 additionally supports verified artifact collection before cleanup and explicit partial-cleanup recovery, described below. Without these options, local files remain a reason to refuse cleanup.

Preserve work when cleanup refuses; do not bypass it with force removal. Cleanup uses `git worktree remove` and optional `git branch -d`, with no force flags. It does not prune unrelated worktrees or recursively delete the operational directory.

Single-lane execution preserves the existing agent-context response. Single-lane preview returns `id`, `status`, `actions`, `reasons`, and, when resolved, `branch_commit` and `target_commit`. Bulk execution/preview returns `format: "lane-cleanup-report/v1"`, `dry_run`, `delete_branch`, `status`, `counts`, and `lanes`:

| Per-lane status | Meaning |
|---|---|
| `ready` | Preview passed; `actions` describes the proposed work |
| `cleaned` | This bulk execution completed cleanup |
| `already-cleaned` | No requested cleanup actions remain |
| `blocked` | Preserved for inspection; read `reasons` and `error_code` |

`actions` lists planned operations (`remove-worktree`, `delete-branch`, `record-cleanup`), not a guarantee that every operation ran. A bulk result has `status: "preview"` or `"cleaned"` when no lane is blocked, and `"blocked"` otherwise. Completed preview/bulk checks use `ok:true`; any blocked lane produces exit 12. Do not use `ok` alone. Fatal errors outside the per-lane checks retain the usual error envelope and exit codes.

Bulk cleanup continues past blocked lanes and can make partial progress. Inspect every per-lane result; it is not a transaction. Worktree removal is recorded before optional branch deletion, allowing a failed branch deletion to be retried. Repeating completed cleanup is a no-op, and `clean <id> --delete-branch` can later remove an unchanged retained branch. If interruption or a filesystem error occurs between a Git operation and its manifest update, inspect Git and the manifest before retrying.

Integrating a replay does not by itself include the source branch's commits in parent ancestry. Retain the source if normal cleanup refuses it. Manifests, receipts, conflict packets, preflights, plans, and replay records remain as history. Prefer a new lane ID for follow-up work.

## Status and Artifacts

Use `lane --project <repo> status` to discover lanes and `context <id>` to recover their handoff information. Lifecycle labels include `empty`, `active`, `dirty`, `integrated`, and `cleaned`; confirm current Git state for completion decisions rather than relying on a label alone.

Detailed records live under `.agent-tools/lane/`:

| Directory | Contents |
|---|---|
| `manifests/` | Pinned base, assigned branch/worktree, scope, lifecycle metadata |
| `preflights/` | Single-lane checks |
| `conflicts/` | Conflict packets and change context |
| `plans/` | Ordered virtual integration results |
| `replays/` | Source context for resolution lanes |
| `receipts/` | Completed integration records |
| `collections/` | Rust 0.4 artifact inventory, destination hashes and recovery records |

Read returned artifact paths when needed for the next decision. `packet <id>` can explicitly produce a conflict-context packet. Preserve relevant records on failure so that the next action follows observed state.

## Rust 0.4: Collection, Recovery, Status and Contracts

These commands require `schema.data.implementation == "rust"` and version 0.4 or later. Check the executable's schema: building a checkout does not update an installed CLI. Existing v1 manifests, config and Git state remain readable. Keep exclusive write ownership through integration, collection and cleanup; do not mix executables concurrently on the same lane.

### Preserve build outputs before cleanup

```text
lane --project <repo> artifacts parser
lane --project <repo> collect parser --dest <archive-directory>
lane --project <repo> clean parser --dry-run --collection <returned-receipt> --delete-branch
lane --project <repo> clean parser --collection <returned-receipt> --delete-branch
```

`artifacts` inventories ignored and untracked files under `out/` and `target/` by default. Repeat `--root <relative-path>` to select other roots. Tracked files are preserved by Git and are not collection candidates. Each entry contains a repository-relative path, byte size, SHA-256 and whether it is ignored. Inventory is read-only and does not initialize state.

`collect` copies files into a fresh lane-specific subdirectory under `--dest`. It never deletes originals or overwrites a prior collection. The destination must be outside managed worktrees. A durable receipt records the source lane tip, destination path and SHA-256 for each file. A result is usable for cleanup only when `status` is `verified`; failed copies preserve originals and return the receipt and partial destination for inspection. Lane does not execute build or verification commands.

`clean --collection` explicitly authorizes removal of only those local files whose source and collected destination hashes still match. The lane tip must still match the receipt. Source/destination changes, missing files without a cleanup record, uncollected local files, junctions, symlinks or unresolved validation conditions block deletion. All collection entries are verified before removing any source file, and each source is checked again immediately before removal. A collection made before a later worker commit must be refreshed. Keep the workers and all build/log writers stopped: hashing is not a filesystem transaction.

Collection is per lane. `clean --all` does not accept a shared receipt and preserves lanes with uncollected artifacts. Use explicit per-lane cleanup when receipts are required. Manifests and collection receipts remain as history; archive directories are never removed by cleanup.

### Inspect and resume partial cleanup

On an execution failure, inspect `error.details.cleanup`, the manifest's `cleanup_progress`, and `status <id>`. The failure records whether the path exists, whether Git still registers the worktree, remaining files, branch presence, completed artifact removals and the next action. `cleanup-partial` is a Rust lifecycle label for a started cleanup without a completion record.

```text
lane --project <repo> status parser
lane --project <repo> clean parser --dry-run --resume --collection <receipt> --delete-branch
lane --project <repo> clean parser --resume --collection <receipt> --delete-branch
```

Omit `--collection` for lanes with no artifacts. `--resume` may remove an **empty, unregistered directory at the exact managed path**, only when a recorded worktree-removal attempt exists and the lane tip remains unchanged and integrated. It uses nonrecursive directory removal. This covers a Windows lock failure where Git removed the files and registration but could not remove the directory. It also reconciles an artifact deletion interrupted between removal and its completion record, using the recorded pending path, matching lane tip and verified archive.

A nonempty/unregistered path, a missing but registered worktree, a changed/reused branch, an uncollected file, or a junction still requires inspection and remains preserved. Restore interrupted registered worktrees through an explicit Git recovery decision; Lane does not prune unrelated worktrees or force deletion. Branch-deletion failures retain the worktree-removal record and can be retried normally. Interruption returns exit 130 when a console interrupt can be handled; forcibly terminating a process may produce no JSON. Re-read state after any interruption.

### Filter status and request concise JSON

```text
lane --project <repo> status --active --short
lane --project <repo> status --unintegrated --campaign build
lane --project <repo> status --cleanup-pending --owner agent-a --short --limit 20 --offset 0
lane --project <repo> status --state integrated --state cleanup-partial
```

`--active` selects `empty`, `active` and `dirty` lanes; `--unintegrated` selects lanes not integrated and not cleaned; `--cleanup-pending` selects empty/integrated lanes with worktrees and partial cleanups. Repeated `--state` values are alternatives; other filters combine with AND. Owner/campaign metadata is filtered before running Git checks. `--short` returns only `id`, `state`, `cwd`, `tip`, `dirty` and `owner`, without computing diffs or scope validation. Use `validate` and cleanup preview for admission decisions. Filtered/concise/paginated results include `total` (matches before pagination), `offset` and `limit`. Default status retains the v1 full result. Completed cleaned history uses its recorded cleanup tip.

### Extend the cooperative worker contract

Create an UTF-8 JSON object and pass it with `spawn --contract <file>` or `contract <id> --file <file>`:

```json
{"owner":"agent-a","campaign":"build","symbols":["api::parse"],"dependencies":["<full-commit-id>"],"required_checks":["unit-tests"],"required_artifacts":["out/build.log"]}
```

`spawn --owner <name> --campaign <name>` sets those metadata fields directly. `contract <id>` reads the contract; `--file` replaces it, preserving unrelated manifest metadata. `context` includes the contract when present. Symbol ownership is descriptive; Lane does not analyze or enforce symbol-level edits. Dependencies must be full commit IDs and ancestors of the lane tip. Required checks block validation/integration/cleanup until passing **parent attestations for the current tip** exist. Review and run the checks first, then record the evidence:

```text
lane --project <repo> attest parser --check unit-tests --result passed --evidence <log-path-or-description>
lane --project <repo> contract parser --freeze
```

`attest` records a result, evidence text, timestamp and current lane tip; it does not run tests or independently verify evidence. A new commit invalidates earlier check records. `--result failed` records failure. `--freeze` pins `frozen_commit`; changing the branch afterward fails validation. `contract --unfreeze` removes that pin. Freeze does not stop an external writer. Required artifacts name exact files or nonempty directory prefixes that must appear in a verified cleanup collection. They gate cleanup rather than code integration.

Replay inherits the descriptive contract, dependencies and requirements, resets the freeze, and requires fresh check attestations for the new lane. Normal Git validation, pinned-commit integration and cleanup safety checks still apply. Older Python binaries do not enforce these Rust contract extensions: after adopting contracts, keep the parent on the Rust CLI for validation/integration/cleanup.

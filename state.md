# git-cascade Implementation State

## Current Status

The repository now contains an initial Rust CLI implementation for `git-cascade`.

Implemented so far:

- Repository scaffold copied/adapted from `../languagetool-lsp`.
- Rust workspace with crate `crates/git-cascade`.
- Synchronous Git subprocess wrapper using `std::process::Command`.
- `test-hooks` Cargo feature for internal test hooks, compiled out by default.
- Repository-local storage path handling via `git rev-parse --path-format=absolute --git-common-dir`.
- `PlanName` newtype with unpadded base64url filesystem serialization for named plan files.
- Base64url component encoding for branch-derived ref/file components.
- Typed YAML plan schema where anchor/dependent data is represented by a `kind` enum.
- Standalone plan validation for schema shape, graph consistency, Git object existence, commit ranges, parent reachability, and apply-time branch ref checks.
- Parent-before-child topological ordering for future apply execution.
- `git cascade apply <name> --new-tip <ref> --dry-run` command preview.
- `apply --dry-run --strategy move-to-heads` base preview.
- Mutating `git cascade apply <name> --new-tip <ref>` for clean linear branch stacks.
- Repository-wide apply lock creation through `<git-common-dir>/cascade/state.yaml`.
- Mutating operations hold an exclusive write lock on the open `state.yaml` file for their full duration.
- Active apply state uses typed enum values for operation, phase, and strategy, and stores the plan name as a required `PlanName`.
- Plan IDs are UUIDs.
- Temporary worktree replay under `<git-common-dir>/cascade/worktrees/<plan-id>`.
- Temporary rewritten branch refs under `refs/cascade/tmp/<plan-id>/<encoded-branch>`.
- Final dependent branch promotion through a single `git update-ref --stdin` transaction.
- Success cleanup for state file, temporary refs, and temporary worktree.
- Durable replay state updates during mutating apply, including current branch/commit, mappings, completed temp refs, and pending branches.
- `git cascade status` for reporting active operation state.
- `git cascade abort` for aborting preserved conflict state and cleaning temp refs/worktrees.
- `git cascade continue` for completing a resolved cherry-pick conflict and resuming the cascade.
- Cleanup marks state as `phase: deleting` before deleting temp refs/worktrees/state.
- Loading a `phase: deleting` state continues cleanup and then behaves as if no active state exists.
- Feature-gated `before-final-update` test hook for ref-safety testing.
- `git cascade list` for named plans.
- `git cascade show <name>` for named plans.
- Apply only supports named plans stored under the repository Git common-dir; exported/path-based plans are intentionally unsupported.
- `git cascade plan <name> --old-base <ref> --old-tip <ref>` for initial linear-stack planning.
- `git cascade plan --replace` overwrite behavior.
- Command and flag help text exposed through Clap help output.
- `git cascade completions <shell>` for shell completion script generation.
- Real-Git integration test harness using temporary repositories.

## Current Plan Generation Behavior

`git cascade plan` currently:

- Resolves `--old-tip` as the old root range tip and resolves `--old-base` by taking `merge-base(<old-tip>, <old-base>)`.
- Discovers dependent branches by their attachment points to the anchor or already discovered dependents.
- Captures owned commits with `git rev-list --reverse <base>..<tip>`.
- Rejects merge commits in captured ranges.
- Discovers dependent local branches by finding fork points inside already-captured parent commits.
- Preserves intermediate fork points in the generated plan.
- Writes plans to `<git-common-dir>/cascade/plans/<base64url-plan-name>.yaml`.
- Refuses to overwrite existing plans unless `--replace` is passed.
- Refuses to create a plan while `<git-common-dir>/cascade/state.yaml` exists.

## Test Coverage

Current tests include:

- Unit tests for base64url encoding.
- Unit tests for plan-key filesystem encoding/decoding.
- Unit tests for storage path construction.
- Real-Git integration tests for `list` and `show`.
- Real-Git integration tests for plan names containing path separators.
- Real-Git integration tests for tag and full `refs/heads/*` old tips.
- Real-Git integration tests for linear stack plan generation.
- Real-Git integration tests proving an advanced default branch is not treated as a dependent.
- Real-Git integration tests for explicit `--base` planning.
- Real-Git integration tests for intermediate fork-point preservation.
- Real-Git integration tests for `--replace` behavior.
- Real-Git integration tests for refusing plan creation while state exists.
- Real-Git integration tests for rejecting merge commits.
- Unit tests for plan topological ordering.
- Real-Git integration tests for generated plan validation.
- Real-Git integration tests for tampered plan rejection.
- Real-Git integration tests for apply-mode validation rejecting moved dependent branches.
- Real-Git integration tests for `apply --dry-run` command output.
- Real-Git integration tests for `apply --dry-run --strategy move-to-heads` base descriptions.
- Real-Git integration tests proving dry-run leaves refs/state/temp refs unchanged.
- Real-Git integration tests proving dry-run refuses moved dependent branches.
- Real-Git integration tests for mutating apply on a clean linear stack.
- Real-Git integration tests for mutating apply preserving intermediate fork points.
- Real-Git integration tests for mutating apply with `--strategy move-to-heads`.
- Real-Git integration tests for mutating apply refusing an existing state file.
- Real-Git integration tests for mutating apply refusing moved dependent branches.
- Real-Git integration tests for conflict safety: permanent refs unchanged and state retained.
- Real-Git integration tests for `status` with and without active state.
- Real-Git integration tests for `abort` cleanup after conflict.
- Real-Git integration tests for `abort` without active state.
- Real-Git integration tests for `continue` after manual conflict resolution.
- Real-Git integration tests for `continue` refusing unresolved conflicts.
- Real-Git integration tests for `continue` without active state.
- Feature-gated real-Git integration test proving final update refuses a moved replacement tip ref.
- Real-Git integration assertion that conflict state records the plan-id worktree path.
- Real-Git integration tests for abort tolerating already-deleted worktree files.
- Real-Git integration tests for `phase: deleting` state cleanup on status.
- CLI help tests covering commands and apply strategy options.
- CLI tests for shell completion help and Bash completion generation.
- CLI invalid-input tests for missing plan name and invalid `--strategy`.

Verified commands:

```sh
cargo fmt --all
cargo make ci
cargo test -p git-cascade --features test-hooks
cargo clippy -p git-cascade --features test-hooks --all-targets --no-deps -- -D warnings
```

## Known Limitations

- Plan generation supports linear ranges only and rejects merge commits.
- Dependent branch discovery is first-pass and may need more edge-case coverage.
- Remote-tracking branches are not updated; v1 should continue to target local branches only.
- Conflict continuation is implemented for named plans and resolved cherry-pick conflicts.
- `apply --dry-run` prints the Git operations that would run without promising conflict-free replay.
- Exported/path-based plans are not supported.
- Release workflow will only become fully usable once the `git-cascade` package is published through normal release flow.

## Next Steps

1. Add crash/restart tests for continuing from preserved state.
2. Harden cleanup behavior when temp refs or worktrees already exist.
3. Add richer status output for completed mappings/temp refs if needed.
4. Add tests for continuation that hits a second conflict on a later commit.

## Recommended Immediate Next Step

Harden recovery and edge-case coverage around continuation.

Rationale:

- Mutating apply now handles clean stacks and safely stops on conflict with permanent refs unchanged.
- Users can inspect active state with `status` and clean it with `abort`.
- The primary conflict lifecycle is now implemented: apply conflict, status, abort, and continue.
- The remaining risk is robustness across crashes, repeated conflicts, stale refs, and pre-existing temp artifacts.

Suggested hardening scope:

- Add tests for interruption after temp refs are written.
- Add tests for continuation that resolves one conflict and later hits another.
- Add tests for stale temp worktree/ref cleanup behavior.

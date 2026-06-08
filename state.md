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
- Typed YAML plan schema where anchor/dependent status is inferred from `parent: null` instead of a separate role field.
- Standalone plan validation for schema shape, graph consistency, Git object existence, commit ranges, parent reachability, and apply-time branch ref checks.
- Parent-before-child topological ordering for future apply execution.
- `git cascade apply --name <name> --new-anchor <ref> --dry-run` command preview.
- `apply --dry-run --move-to-heads` base preview.
- Mutating `git cascade apply --name <name> --new-anchor <ref>` for clean linear branch stacks.
- Repository-wide apply lock creation through `<git-common-dir>/cascade/state.yaml`.
- Temporary worktree replay under `<git-common-dir>/cascade/worktrees/<plan-id>`.
- Temporary rewritten branch refs under `refs/cascade/tmp/<plan-id>/<encoded-branch>`.
- Final dependent branch promotion through a single `git update-ref --stdin` transaction.
- Success cleanup for state file, temporary refs, and temporary worktree.
- `git cascade list` for named plans.
- `git cascade show --name <name>` for named plans.
- `git cascade plan <anchor-branch> --name <name>` for initial linear-stack planning.
- `git cascade plan --replace` overwrite behavior.
- `git cascade plan --main <ref>` explicit main/base reference selection.
- Implicit base selection from `refs/remotes/origin/HEAD`, then local `main`, then local `master`.
- Real-Git integration test harness using temporary repositories.

## Current Plan Generation Behavior

`git cascade plan` currently:

- Resolves the anchor as a local branch under `refs/heads`.
- Infers the anchor old base from `--main`, `origin/HEAD`, local `main`, or local `master`.
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
- Unit tests for plan-name filesystem encoding/decoding.
- Unit tests for storage path construction.
- Real-Git integration tests for `list` and `show`.
- Real-Git integration tests for plan names containing path separators and spaces.
- Real-Git integration tests for linear stack plan generation.
- Real-Git integration tests for intermediate fork-point preservation.
- Real-Git integration tests for `--replace` behavior.
- Real-Git integration tests for refusing plan creation while state exists.
- Real-Git integration tests for rejecting merge commits.
- Real-Git integration tests for `--main` base selection.
- Real-Git integration tests for origin default branch base selection through `refs/remotes/origin/HEAD`.
- Unit tests for plan topological ordering.
- Real-Git integration tests for generated plan validation.
- Real-Git integration tests for tampered plan rejection.
- Real-Git integration tests for apply-mode validation rejecting moved dependent branches.
- Real-Git integration tests for `apply --dry-run` command output.
- Real-Git integration tests for `apply --dry-run --move-to-heads` base descriptions.
- Real-Git integration tests proving dry-run leaves refs/state/temp refs unchanged.
- Real-Git integration tests proving dry-run refuses moved dependent branches.
- Real-Git integration tests for mutating apply on a clean linear stack.
- Real-Git integration tests for mutating apply preserving intermediate fork points.
- Real-Git integration tests for mutating apply with `--move-to-heads`.
- Real-Git integration tests for mutating apply refusing an existing state file.
- Real-Git integration tests for mutating apply refusing moved dependent branches.
- Real-Git integration tests for conflict safety: permanent refs unchanged and state retained.

Verified commands:

```sh
cargo fmt --all
cargo make ci
cargo test -p git-cascade --features test-hooks
cargo clippy -p git-cascade --features test-hooks --all-targets --no-deps -- -D warnings
```

## Known Limitations

- `git cascade continue`, `abort`, and `status` are not implemented yet.
- Plan generation supports linear ranges only and rejects merge commits.
- Dependent branch discovery is first-pass and may need more edge-case coverage.
- Remote-tracking branches are not updated; v1 should continue to target local branches only.
- Conflict recovery is not implemented yet; apply stops with state/worktree preserved.
- `apply --dry-run` prints the Git operations that would run without promising conflict-free replay.
- Release workflow will only become fully usable once the `git-cascade` package is published through normal release flow.

## Next Steps

1. Implement `git cascade status` for active apply state.
2. Implement `git cascade abort` for cleaning preserved conflict state/worktrees/temp refs.
3. Implement `git cascade continue` for completing conflict resolutions.
4. Persist state updates during replay instead of only writing initial state.
5. Add crash/restart tests for continuing from preserved state.
6. Add explicit final anchor-ref verification tests.
7. Add tests for `--plan <path>` apply flows.
8. Harden cleanup behavior when temp refs or worktrees already exist.

## Recommended Immediate Next Step

Implement `status`, `abort`, and `continue` around preserved apply state.

Rationale:

- Mutating apply now handles clean stacks and safely stops on conflict with permanent refs unchanged.
- The next risk is recoverability: users need supported commands to inspect, abort, and continue preserved conflict state.
- State updates during replay should become durable enough for crash/restart scenarios.

Suggested recovery scope:

- `status` should read `state.yaml` and report phase, current branch/commit, worktree, completed refs, and pending branches.
- `abort` should abort any in-progress sequencer in the temp worktree, remove safe temp refs/worktrees, and remove `state.yaml`.
- `continue` should validate refs/state, ensure the conflict worktree has no unmerged entries, complete the current cherry-pick, and resume replay.
- Tests should cover conflict, status output, abort cleanup, and continue after manual resolution.

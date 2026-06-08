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

Verified commands:

```sh
cargo fmt --all
cargo make ci
cargo test -p git-cascade --features test-hooks
cargo clippy -p git-cascade --features test-hooks --all-targets --no-deps -- -D warnings
```

## Known Limitations

- Mutating `git cascade apply` is not implemented yet.
- `git cascade continue`, `abort`, and `status` are not implemented yet.
- Plan generation supports linear ranges only and rejects merge commits.
- Dependent branch discovery is first-pass and may need more edge-case coverage.
- Remote-tracking branches are not updated; v1 should continue to target local branches only.
- Temporary ref naming with base64url branch components is not wired into apply yet.
- No atomic state-file lock creation is implemented yet because mutating apply operations are not implemented.
- No final `git update-ref --stdin` transaction is implemented yet.
- `apply --dry-run` prints the Git operations that would run without promising conflict-free replay.
- Release workflow will only become fully usable once the `git-cascade` package is published through normal release flow.

## Next Steps

1. Add state-file model and atomic state-file creation helpers for non-dry-run apply.
2. Add temporary worktree path management and cleanup helpers.
3. Implement replay into a temporary worktree using cherry-pick.
4. Store replay results under safe temporary refs using encoded branch components.
5. Implement final atomic dependent-branch update with `git update-ref --stdin`.
6. Add integration tests for successful mutating `apply` on a linear stack.
7. Add integration tests for preserved intermediate fork points during mutating `apply`.
8. Add integration tests for `--move-to-heads` mutating apply.
9. Add conflict detection that leaves permanent refs unchanged and writes durable state.
10. Implement `status`, `abort`, and `continue`.

## Recommended Immediate Next Step

Implement state-file locking and temporary worktree replay for mutating apply.

Rationale:

- Dry-run now exercises plan loading, validation, anchor resolution, dependency ordering, and replay-base selection without mutating repository state.
- The next risk is safe mutation: acquiring the repository-wide lock, creating temporary worktrees, and ensuring permanent refs remain unchanged until final transaction commit.
- Implementing state before replay keeps conflict and crash recovery paths aligned with the design.

Suggested mutating apply scope:

- Create `<git-common-dir>/cascade/state.yaml` with exclusive-create semantics after preflight validation.
- Create or reset a temporary worktree under `<git-common-dir>/cascade/worktrees/<plan-id>`.
- Replay commits with `git cherry-pick` in parent-before-child order.
- Write each completed branch tip to `refs/cascade/tmp/<plan-id>/<encoded-branch>`.
- Preserve old-to-new commit mappings needed for default fork-point preservation.
- Stop on conflict with permanent refs unchanged and durable state written.
- Promote completed temporary refs with one final `git update-ref --stdin` transaction only after every branch replay succeeds.

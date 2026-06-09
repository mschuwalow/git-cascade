# Cascade Rebase Plan

## Tool Name

The proposed CLI name is `git-cascade`.

When installed on `PATH`, Git exposes executables named `git-<command>` as Git subcommands, so users invoke it as:

```bash
git cascade <subcommand>
```

This keeps the UX Git-native while leaving the actual binary name unambiguous for packaging.

## Purpose

A cascade rebase plan is an immutable snapshot of a dependent branch stack captured before the stack is disrupted by a manual rebase, merge, squash, or other history mutation.

The plan exists so the tool does not need to infer the old stack structure after refs have already been moved. It records the old branch tips, old parent-child relationships, and exact per-branch commit ranges while that information is still reliable.

The intended workflow is:

```bash
git cascade plan stack --old-tip my-branch
git rebase --onto origin/main 3e1e56f91cad6cc45281f86849ee9e727ccac340
git cascade apply stack --new-tip <new-tip>
```

The user manually rebases one branch. The tool then uses the saved plan to cascade that rewrite through all dependent branches.

## Problem

Stacked pull requests often look like this:

```text
main
  \
   pr-1
     \
      pr-2
        \
         pr-3
```

After `pr-1` is merged, rebased, squash-merged, or otherwise rewritten, the remaining dependent branches still point at the old history:

```text
main
  \
   new-pr-1-tip

old-pr-1-tip
  \
   pr-2
     \
      pr-3
```

At that point, normal Git ancestry may no longer contain enough information to safely infer the original stack. The purpose of the plan is to capture the old structure before this happens.

## Core Idea

The process is intentionally split into two phases:

1. Plan generation: read-only Git inspection before mutation.
2. Plan application: destructive branch updates after the user manually rewrites or selects the replacement root tip.

The plan describes transformations over commits, not branches. The anchor can be any Git ref or commit-ish; dependent branch names are output targets and safety checks. They are not used during apply to rediscover history.

## Primary Workflow

### 1. Create The Plan

```bash
git cascade plan stack --old-tip my-branch
```

This command is read-only. It inspects the current Git repository and captures the cascade rooted at the old range `old-base..old-tip`. The `--old-tip` value may be a branch, tag, full ref, or commit-ish.

The plan is stored in the repository's Git common directory keyed by the explicit plan name. Users do not normally pass plan files around manually.

The generated plan records:

- The plan name.
- The old root range base used to bound direct dependent discovery.
- The old root range tip before mutation.
- Every dependent branch in the cascade.
- Each dependent branch's old tip.
- Each dependent branch's old base.
- Each dependent branch's exact owned commits, oldest to newest.
- Explicit parent-child dependency edges.

No refs are updated. No rebase is executed. No temporary branches are created.

### 2. Rebase The Anchor Manually

```bash
git switch my-branch
git rebase --onto origin/main 3e1e56f91cad6cc45281f86849ee9e727ccac340
```

The user performs the anchor rewrite manually. This is intentionally outside the plan generation phase.

This step may break the old relationships between the anchor and its dependent branches. That is acceptable because the old relationships have already been captured in the plan.

### 3. Apply The Plan

```bash
git cascade apply stack --new-tip <new-tip>
```

Apply requires both the plan name and an explicit replacement tip. The tool resolves `--new-tip` exactly once and treats the resolved commit as the replacement for the old root range tip.

Then it replays each dependent branch's saved commit range onto the rewritten equivalent of its original fork point.

If a dependent branch gained new linear commits after planning, apply replays those appended commits after the saved range and compares the final ref update against the apply-time branch tip. If the saved planned range is no longer reachable from the dependent branch tip, apply refuses because a planned commit was rewritten or removed.

For dependencies between non-anchor branches, the default behavior preserves the old topology exactly: if a child branch originally forked from the middle of its parent branch, the rewritten child forks from the corresponding rewritten parent commit.

If the user wants the simpler behavior of moving every branch to the tip of its rewritten parent, they can pass `--strategy move-to-planned-tips` or `--strategy move-to-current-tips`.

For example:

```text
new-my-branch-tip
  \
   rewritten-pr-2
     \
      rewritten-pr-3
```

The apply step does not rebase or update the old root tip. The replacement root tip was already created manually.

## Apply Modes

### Required Replacement Tip

The replacement root tip is always explicit:

```bash
git cascade apply stack --new-tip origin/main
```

or:

```bash
git cascade apply stack --new-tip <commit-sha>
```

When `--new-tip` is provided, the tool resolves that ref or commit exactly once at the start of execution and uses the resolved object ID as the new root tip.

There is no implicit fallback to the current tip of the original old tip ref. If the desired new tip is a manually rebased branch, pass it explicitly:

```bash
git cascade apply stack --new-tip my-branch
```

### Move To Tips

The default apply mode preserves fork points between non-anchor branches.

The `--strategy move-to-planned-tips` option replays each branch onto its parent's rewritten planned tip:

```bash
git cascade apply stack --new-tip my-branch --strategy move-to-planned-tips
```

The `--strategy move-to-current-tips` option replays each branch onto its parent's rewritten apply-time tip, including linear commits appended to the parent after planning:

```bash
git cascade apply stack --new-tip my-branch --strategy move-to-current-tips
```

With either option, every dependent branch is moved from any intermediate old parent commit to a rewritten parent tip.

## Concepts

### Old Root Range

The old root range is selected when creating the plan with `--old-tip` and an inferred or explicit `--old-base`. `--old-base` is combined with `--old-tip` through `merge-base`, so `--old-base main --old-tip pr-1` remains stable even if `main` has advanced.

The apply step does not rewrite the old tip. It only uses `--new-tip` as the replacement commit for replaying dependents.

### Dependent Branch

A dependent branch is a branch whose old base was some commit reachable from another branch in the captured cascade.

During apply, its saved commits are replayed onto the selected rewritten base for its parent relationship.

### Tip

The persisted `tip` is the exact branch tip at plan creation time.

For dependent branches, this is used as the expected old value during the final ref update. This prevents the tool from overwriting work added after the plan was created.

### Base

The persisted `base` is the exact commit before a branch's owned commits begin.

For a dependent branch, `base` must be reachable from the tip of its declared parent, but it does not need to equal the parent's tip. This supports branches that were created from an intermediate parent commit.

Version 1 preserves fork points between non-anchor branches by default. If a dependent branch originally forked from an intermediate commit in another dependent branch, the apply phase replays it onto the rewritten equivalent of that intermediate commit.

The `--strategy move-to-planned-tips` and `--strategy move-to-current-tips` options select simpler strategies that replay every dependent branch onto a rewritten tip of its parent.

### Commits

`commits` is the exact list of commits owned by a branch, ordered oldest to newest.

Version 1 supports linear commit ranges only. Merge commits are rejected unless a later schema version defines merge replay semantics.

## Plan File Contract

The plan is machine-readable and immutable after creation.

Plans are named and stored inside the repository's Git common directory. Version 1 does not support exported or path-based plans.

Important rules:

- All commit identifiers are full object IDs, not symbolic refs.
- Dependent branch ranges are stored as explicit commit lists.
- Dependency edges are explicit.
- Execution order is derived from the dependency graph.
- Apply does not use merge-base inference to rediscover the old stack.
- The plan does not store the new base by default.

The new base is intentionally not stored because the plan is created before the manual rewrite happens. The replacement tip is supplied explicitly with `--new-tip` during apply.

## Repository Storage

`git-cascade` stores repository-local data under the Git common directory:

```bash
git rev-parse --git-common-dir
```

Use the Git common directory rather than assuming `.git` directly because linked worktrees have per-worktree `.git` files while refs and shared repository data live in the common directory.

Repository-local storage layout:

```text
<git-common-dir>/cascade/
  plans/
    <base64url-plan-name>.yaml
  state.yaml
  worktrees/
    <plan-id>/
```

`plans/<base64url-plan-name>.yaml` stores immutable named plans. The plan name is encoded for filesystem safety.

`state.yaml` stores the single active cascade operation and acts as the repository-wide cascade lock.

`worktrees/<plan-id>/` stores the temporary Git worktree used for isolated replay and conflict resolution for the active operation.

### Named Plans

Named plans are the default interface:

```bash
git cascade plan stack --old-tip my-branch
git cascade apply stack --new-tip my-branch
```

Plan names are encoded as unpadded base64url for filesystem storage.

Creating a plan:

- Writes `<git-common-dir>/cascade/plans/<base64url-plan-name>.yaml`.
- Refuses to overwrite an existing plan unless `--replace` is passed.
- Refuses to run while `state.yaml` exists.
- Stores a stable `plan_id` inside the plan.
- Does not store apply-time behavior such as `--strategy move-to-current-tips`.

Useful plan management commands:

```bash
git cascade list
git cascade show stack
git cascade delete stack
```

Shell completion scripts are generated by Clap:

```bash
git cascade completions bash
git cascade completions zsh
git cascade completions fish
```

### State As Lock

Only one cascade flow may be active in a repository at a time. The active operation state file is also the lock file:

```text
<git-common-dir>/cascade/state.yaml
```

If `state.yaml` exists, `git-cascade` must refuse to start any new mutating cascade operation.

Allowed while `state.yaml` exists:

- `git cascade status`
- `git cascade continue`
- `git cascade abort`
- Read-only plan inspection commands such as `list` and `show`

Refused while `state.yaml` exists:

- `git cascade plan ...`
- `git cascade apply ...`
- `git cascade delete ...`
- Any command that creates, updates, or deletes plans, refs, temporary refs, or worktrees outside the active operation

Mutating operations take an exclusive lock on `<git-common-dir>/cascade/state.lock` before reading or writing `state.yaml`. State writes should be atomic replacements of `state.yaml` so a crash does not truncate the last valid recovery state.

The state file should contain enough information to abort, diagnose stale state, and resume explicit recovery phases. Normal in-process replay progress is not durably resumed; detailed replay state is written only when apply stops for a conflict or reaches final ref update.

```yaml
version: 1
phase: conflict
plan_name: permissions-stack
plan_id: "550e8400-e29b-41d4-a716-446655440000"
started_at: "2026-06-08T14:00:00Z"
updated_at: "2026-06-08T14:05:12Z"
pid: 12345

new_tip: "abcdefabcdefabcdefabcdefabcdefabcdefabcd"

strategy: preserve-fork-points

current:
  branch: agent-permissions-9
  commit: "3333333333333333333333333333333333333333"
  worktree: "<git-common-dir>/cascade/worktrees/550e8400-e29b-41d4-a716-446655440000"

worktree:
  mode: temporary
  path: "<git-common-dir>/cascade/worktrees/550e8400-e29b-41d4-a716-446655440000"

completed:
  temp_refs:
    - refs/cascade/tmp/550e8400-e29b-41d4-a716-446655440000/agent-permissions-8

cleanup:
  delete_plan: false

mappings:
  "9c501c50a412ee5e28b89f5cb80ff5957b6b4a42": "abcdefabcdefabcdefabcdefabcdefabcdefabcd"
  "3333333333333333333333333333333333333333": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

pending:
  branches:
    - agent-permissions-10
```

Stale state should be handled conservatively. A recovery command may exist, but it should verify that no recorded process or Git sequencer operation is still active before removing `state.yaml`.

## Plan Schema

The examples below use YAML, but the same schema could be serialized as JSON if desired.

```yaml
version: 1
plan_id: "550e8400-e29b-41d4-a716-446655440000"
generated_at: "2026-06-08T14:00:00Z"

repository:
  git_dir: ".git"
  head_at_generation: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

source:
  name: "permissions-stack"
  base: "1111111111111111111111111111111111111111"
  tip: "9c501c50a412ee5e28b89f5cb80ff5957b6b4a42"

nodes:
  - branch: "agent-permissions-9"
    tip: "5b58f121371d5e79ab4c769bbe8c7867958f939a"
    base: "9c501c50a412ee5e28b89f5cb80ff5957b6b4a42"
    commits:
      - "3333333333333333333333333333333333333333"
      - "5b58f121371d5e79ab4c769bbe8c7867958f939a"

  - branch: "agent-permissions-10"
    tip: "7777777777777777777777777777777777777777"
    base: "5b58f121371d5e79ab4c769bbe8c7867958f939a"
    commits:
      - "5555555555555555555555555555555555555555"
      - "7777777777777777777777777777777777777777"
    parent: "agent-permissions-9"

dependencies:
  - parent: "agent-permissions-9"
    child: "agent-permissions-10"
```

### Required Top-Level Fields

`version` is the plan schema version. This document defines version 1.

`plan_id` is a UUID used for temporary ref namespaces.

`generated_at` is the RFC 3339 timestamp when the plan was created.

`repository` contains optional diagnostic metadata. It must not be required for correctness because plans should not depend on absolute paths.

`source` describes the plan name and old root range.

`nodes` contains the captured branch graph. Every node is a local branch that may be updated by apply. Branches attached directly to the source root range omit `parent`; dependent branches set `parent` to another node branch.

`dependencies` contains explicit parent-child DAG edges.

## Plan Generation

```bash
git cascade plan <name> --old-tip <old-tip-ref> [--old-base <base-ref>]
```

Plan generation is a read-only phase.

It stores the generated plan at `<git-common-dir>/cascade/plans/<base64url-plan-name>.yaml`.

Allowed Git operations include:

- `git rev-parse`
- `git rev-list`
- `git merge-base`
- `git for-each-ref`
- `git show-ref`
- `git log`

Forbidden Git operations include:

- `git rebase`
- `git cherry-pick`
- `git merge`
- `git reset`
- `git update-ref`
- Branch creation.
- Branch deletion.

Generation rules:

- Resolve the anchor and all saved commits to full object IDs.
- Resolve the old root base as `merge-base(<old-tip-ref>, <base-ref>)` when `--old-base` is provided.
- When `--old-base` is omitted, infer the old root base from `origin/HEAD` or the local default branch (`main` then `master`).
- Direct children of the root must fork after the source `base`; branches whose merge-base with the source `tip` is exactly the source `base` are treated as upstream/sibling branches, not dependents.
- Reject merge commits unless merge replay is explicitly supported by a later schema version.
- Reject a dependent branch if its `base` is not reachable from the `tip` of its declared parent.
- For a non-anchor parent, prefer the parent that owns the dependent branch's `base`, not merely a descendant branch that contains it.
- For default fork-point preservation, a dependent branch whose parent is non-anchor must have a `base` that is either the parent's `base` or one of the parent's saved commits.
- Store commits in replay order, oldest to newest.
- Produce the same plan for the same input Git state.

## Manual Rebase Handoff

After the plan is created, the user can freely rebase or otherwise create the replacement root tip.

If the replacement root tip is a branch, expected manual commands look like:

```bash
git switch <replacement-anchor-branch>
git rebase --onto <new-base> <anchor-old-base>
```

Apply requires `--new-tip`. The tool resolves that ref or commit exactly once at the start of execution and uses the resolved commit as the replacement root tip.

There is no implicit use of the original `--old-tip` input. If a manually rebased branch is the desired replacement tip, the user must pass that branch name explicitly as `--new-tip`.

## Apply Execution

```bash
git cascade apply <name> --new-tip <ref-or-commit>
```

Apply-time behavior is selected by flags, not by defaults stored in the plan. With the default `--strategy preserve-fork-points`, apply preserves fork points between non-anchor branches. With `--strategy move-to-planned-tips`, apply replays each dependent branch onto its parent's rewritten planned tip. With `--strategy move-to-current-tips`, apply replays each dependent branch onto its parent's rewritten apply-time tip, including appended commits.

High-level algorithm:

1. Load and validate the plan schema.
2. Refuse to start if `<git-common-dir>/cascade/state.yaml` already exists.
3. Verify every saved old commit still exists in the object database.
4. Require `--new-tip` and resolve the replacement root tip once.
5. Create `<git-common-dir>/cascade/state.yaml` atomically.
6. Verify dependent branch refs still contain their saved `tip` values.
7. Topologically order dependent nodes from parent to child.
8. Initialize the old-to-new mapping for the anchor boundary.
9. Select an apply-time base for each dependent node.
10. Replay each dependent node's commits onto the selected apply-time base.
11. Record an old-to-new mapping for every replayed commit.
12. Store each replay result under `refs/cascade/tmp/<plan-id>/<branch>`.
13. Atomically update all dependent branch refs from old tip to rewritten tip.
14. Delete temporary refs after the final update succeeds.
15. Delete `state.yaml` after successful completion.

Replay base rules for the default apply mode:

- A direct child of the root is replayed onto the replacement root tip.
- A child of another dependent branch is replayed onto the rewritten equivalent of its original `base`.
- The rewritten equivalent is found from the old-to-new commit map produced while replaying the parent branch.
- If the child's `base` equals the parent's `base`, the child is replayed onto the parent's selected apply-time base.
- If the child's `base` is one of the parent's saved commits, the child is replayed onto the rewritten commit produced from that old commit.

Replay base rules for tip-moving strategies:

- A direct child of the root is replayed onto the replacement root tip.
- A child of another dependent branch is replayed onto `refs/cascade/tmp/<plan-id>/<parent-branch>`.

The tip-moving strategies ignore the dependent branch's original offset within the parent during apply. The original `base` is still required for planning and validation because it defines which commits belong to the dependent branch.

Example:

```text
old parent:        A---B---C
                       \
old dependent:          D---E

new parent:        A'--B'--C'
                       \
new dependent:          D'--E'
```

In this example, the dependent branch originally forked from `B`, so default apply replays it onto `B'`.

With `--strategy move-to-planned-tips` or `move-to-current-tips`, the same branch is replayed onto a rewritten parent tip instead:

```text
new parent:        A'--B'--C'
                           \
new dependent:              D'--E'
```

## Replay Mechanism

Apply should not run `git rebase` on the real target branch refs.

Instead, it should use a detached HEAD or temporary worktree at the computed new base, cherry-pick the saved commits in order, and write the result to a temporary ref.

## Old-To-New Commit Mapping

The default apply mode requires an apply-time map from old commits to rewritten commits.

The map is built as replay progresses:

- The replacement tip maps from `source.tip` to the resolved `--new-tip` commit.
- For each dependent node, the selected apply-time base maps from `node.base` to the commit used as the replay base.
- After each saved commit is replayed, the old commit ID maps to the newly created commit ID.
- Descendant branches use this map to find the rewritten equivalent of their `base`.

Example mapping for a preserved fork point:

```text
old parent:        A---B---C
                       \
old dependent:          D---E

mapping:
  A -> A'
  B -> B'
  C -> C'

new parent:        A'--B'--C'
                       \
new dependent:          D'--E'
```

For a non-anchor parent, default apply must fail before replaying the child if the child's `base` cannot be mapped to a rewritten commit. That indicates the plan did not capture enough information to preserve topology exactly.

When a tip-moving strategy is used, descendants do not need old fork-point mappings. They use the selected rewritten tip of their parent.

Temporary refs use this namespace:

```text
refs/cascade/tmp/<plan-id>/
```

Example temporary refs:

```text
refs/cascade/tmp/550e8400-e29b-41d4-a716-446655440000/agent-permissions-9
refs/cascade/tmp/550e8400-e29b-41d4-a716-446655440000/agent-permissions-10
```

Temporary refs are deleted after success. On conflict or failure, they are kept by default so the user can inspect or resume the operation.

## Final Ref Update

Permanent dependent branch refs must not be updated one at a time.

Use a single `git update-ref --stdin` transaction with expected old values:

```text
start
update refs/heads/agent-permissions-9 <new-tip-9> 5b58f121371d5e79ab4c769bbe8c7867958f939a
update refs/heads/agent-permissions-10 <new-tip-10> 7777777777777777777777777777777777777777
prepare
commit
```

Requirements:

- All dependent refs are updated in one prepared transaction.
- Each update includes the expected apply-time branch tip captured before replay.
- `--new-tip` is resolved once when apply starts; final update uses the persisted resolved commit even if the input ref later moves.
- No dependent branch ref is updated if any expected old value does not match.

## Validation

Before replay:

- The plan version is supported.
- `plan_id` is a UUID and therefore safe for use in a ref namespace.
- `--new-tip` is present and resolves to a commit.
- The selected apply strategy is `preserve-fork-points`, `move-to-planned-tips`, or `move-to-current-tips`.
- All `base`, `tip`, and `commits` objects exist locally.
- The dependency graph is acyclic.
- Every dependency references known nodes.
- Every node either omits `parent` as a root node or has a `parent` branch as a dependent node.
- Every root node has a `base` inside `source.base..source.tip`.
- Every dependent node has a parent.
- Every dependent `base` is reachable from its parent `tip`.
- Every node commit list matches `base..tip`, and `tip` matches the last saved commit.
- In the default fork-point-preserving mode, every dependent child `base` is mappable through its parent's replay.
- Every planned branch `tip` is still reachable from the branch's current apply-time tip.

Before final ref update:

- Every node has a temporary rewritten tip.
- Every temporary rewritten tip exists.
- Every branch ref still equals the apply-time tip captured before replay started.
- The persisted resolved `--new-tip` commit is used as the replacement root tip.

## Atomicity And Safety

### Branch Ref Atomicity

Permanent dependent branch refs are not modified until all replay operations have succeeded and the final `update-ref` transaction commits.

### Compare-And-Swap Updates

Every final branch update must include the apply-time branch tip captured before replay. This prevents overwriting user work added to a dependent branch while apply is running.

### Limits Of Atomicity

Git objects, temporary refs, worktrees, index state, and conflict files may be created before success.

The guarantee is not that the repository is completely unchanged after failure. The guarantee is that permanent dependent branch refs are not partially updated.

### Working Tree Isolation

The safest implementation uses a temporary worktree for replay.

If the current worktree is used, the tool must require it to be clean before replay and must clearly report any conflict state it leaves behind.

## Conflict Handling

Conflicts are expected during the apply phase. They should be treated as normal control flow, not as corruption of the plan.

A conflict can happen while replaying any saved commit onto a node's selected apply-time base. In version 1 this replay is implemented as cherry-picking into a detached HEAD or temporary worktree, even though the user-facing operation is conceptually a cascade rebase.

Default behavior when a replay conflict occurs:

- Stop at the first conflicting replay.
- Leave permanent dependent branch refs unchanged.
- Keep completed temporary refs.
- Keep the conflicted temporary worktree for manual resolution.
- Record apply state so the operation can continue later.
- Print the conflicted branch, commit, worktree path, and continue command.

Conflict state should include:

- The `plan_id`.
- The resolved replacement root tip.
- The selected apply strategy.
- The old-to-new commit mappings produced so far.
- The current dependent branch being replayed.
- The current commit being replayed.
- The temporary worktree path.
- Completed temporary refs.
- Pending dependent nodes.

The state is stored at:

```text
<git-common-dir>/cascade/state.yaml
```

The conflict worktree should contain the in-progress replay exactly where Git stopped. The user resolves conflicts using normal Git commands inside that worktree:

```bash
git -C <conflict-worktree> status
git -C <conflict-worktree> add <resolved-files>
```

Continue command:

```bash
git cascade continue
```

Abort command:

```bash
git cascade abort
```

Status command:

```bash
git cascade status
```

Continue behavior:

- Load the active operation from `<git-common-dir>/cascade/state.yaml`.
- Validate that the saved conflict state matches the named plan recorded in state.
- Validate that the saved apply strategy is internally consistent with the recorded operation state.
- If the operation is in `final_update`, retry the final ref transaction using the persisted resolved replacement tip.
- Validate that permanent dependent branch refs still contain their saved `tip` values.
- Validate that the conflict worktree has no unmerged index entries.
- Complete the current replayed commit using the user's resolution.
- Record the old-to-new mapping for the resolved commit.
- Continue replaying the remaining saved commits for the current node.
- Write the completed node tip to `refs/cascade/tmp/<plan-id>/<branch>`.
- Continue with pending dependent nodes in topological order.
- Run the final atomic ref update only after every dependent node has a rewritten tip.

Abort behavior:

- Load the active operation from `<git-common-dir>/cascade/state.yaml`.
- Abort any in-progress Git sequencer operation in the temporary worktree.
- Leave permanent dependent branch refs unchanged.
- Delete temporary refs that are safe to delete, unless the user requests `--keep-temp`.
- Remove temporary worktrees that are safe to remove.
- Remove `<git-common-dir>/cascade/state.yaml` after cleanup succeeds or after reporting preserved leftovers.

If abort cannot safely remove a worktree because it contains unresolved user edits, the tool should leave it in place and print its path.

Conflict safety guarantees:

- A conflict never updates permanent dependent branch refs by itself.
- Completed temporary refs are not promoted until all dependent branches have completed replay.
- Re-running `git cascade continue` is safe after a crash if validation still passes.
- If validation fails during `git cascade continue`, the tool stops before updating permanent refs.

Manual resolution policy:

- The user's conflict resolution becomes part of the rewritten commit for that dependent branch.
- The rewritten commit will have a new object ID.
- Descendant branches replay according to the selected apply strategy: `preserve-fork-points` uses the mapped rewritten fork point, while tip-moving strategies use the selected rewritten parent tip.
- The plan file itself is not modified by conflict resolution.

Non-interactive mode:

- A `--no-conflicts` or `--abort-on-conflict` mode may stop immediately and clean up temporary state instead of preserving a worktree.
- The default interactive mode should preserve enough state for manual resolution and continuation.

Resume after interruption:

- `git cascade continue` resumes from recorded state after either a conflict or an interrupted apply.
- Previously completed temporary refs may be reused after validation.
- Unresolved nodes continue in topological order.
- Final branch refs are still updated only after all nodes have rewritten tips.

The old `resume` wording can exist as an alias, but the primary user-facing command should be `git cascade continue` because conflicts mirror normal `git rebase` and `git cherry-pick` workflows.

## Invariants

- Same plan plus same replacement root tip produces the same replay attempts.
- Apply does not use merge-base inference to rediscover the old stack.
- A dependent branch is never replayed before its parent has a rewritten tip.
- Saved old commits are treated as immutable inputs.
- Branch names identify refs to validate and update, not history to infer from.
- Default apply preserves dependent branch fork points between non-anchor branches.
- Tip-moving strategies may move dependent branches from intermediate old parent commits to rewritten parent tips.
- Version 1 supports linear branch ranges only.

## Implementation Language

`git-cascade` should be implemented in Rust.

This tool is stateful enough that Bash would be fragile. The implementation needs structured parsing, durable state management, atomic file creation, topological sorting, careful subprocess execution, conflict recovery, and transactional ref updates.

Rust is a good fit because it provides:

- A single native CLI binary.
- Strong typed models for plans, operation state, commits, refs, and branch nodes.
- Reliable path and filesystem handling.
- Explicit error propagation for Git command failures.
- Good support for atomic file creation and durable state updates.
- Good testability for repository fixtures and crash/recovery scenarios.

Recommended Rust crates:

- `clap` for CLI parsing.
- `serde`, `serde_yaml`, and optionally `serde_json` for plan and state serialization.
- `camino` or `std::path` with strict UTF-8 boundaries only where needed.
- `tempfile` for temporary files used in atomic writes.
- `thiserror` or `anyhow` for error handling.
- `assert_cmd` and `insta` for CLI and snapshot tests.

The first implementation should shell out to `git` rather than embedding a Git library. Git is already the source of truth for rebase/cherry-pick behavior, ref transactions, worktrees, and repository discovery.

Critical implementation requirements:

- Use `git rev-parse --git-common-dir` to locate repository-local storage.
- Create `state.yaml` with exclusive-create semantics.
- Write updates to state through temporary files followed by atomic rename.
- Treat branch refs as compare-and-swap targets using expected old object IDs.
- Use `git update-ref --stdin` for the final ref transaction.
- Prefer temporary worktrees for replay and conflict resolution.
- Preserve enough state for `git cascade continue`, `git cascade abort`, and `git cascade status` after process interruption.

## Out Of Scope For Version 1

- Replaying merge commits.
- Preserving merge topology inside a branch range.
- GitHub or GitLab PR retargeting.
- Guessing stack structure after refs have moved.
- Using mutable refs as stored commit identities.
- Updating the replacement root tip during apply.

## Why This Solves The Target Problem

In the target workflow, the important old structure still exists before the user rewrites or selects the replacement root tip.

The plan captures that structure first. After the manual rebase breaks normal ancestry relationships, apply does not need to guess what used to depend on what.

It replays each saved dependent branch range onto the selected rewritten base and then updates all dependent branch refs with compare-and-swap safety.

The result is a practical cascade rebase flow:

```text
old stack before mutation
  -> saved immutable plan
  -> manual anchor rebase
  -> deterministic dependent branch replay
  -> atomic dependent branch ref update
```

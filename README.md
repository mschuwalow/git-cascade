# git-cascade

Git-native CLI for planning and applying cascade rebases across dependent local branch stacks.

When the `git-cascade` binary is installed on `PATH`, Git exposes it as:

```sh
git cascade <command>
```

Pick the command based on what changed in your branch stack:

```sh
git cascade sync
git cascade restack [branch]
git cascade landed <old-tip> [--onto <ref>]
```

Use `git cascade replay --old-base <ref> --old-tip <ref> --new-tip <ref>` when none of those workflows fit and you can name the exact before/after refs. The lower-level `git cascade plan ...` commands are for power users who need to inspect, save, or apply plans manually.

## Common Use Cases

`git-cascade` is built for local branch stacks where one branch is the base for another:

```text
main -- A
         \
pr-1      B -- C
                \
pr-2             D -- E
                       \
pr-3                    F
```

The common situations are:

- Your target branch advanced, and local branch stacks should catch up.
- A parent branch gained commits, and child branches should follow it.
- A parent branch landed, and child branches should now start from the landed result.

### Target Branch Advanced

Use `sync` after updating your target branch when your local branch stacks should catch up to it.

For example, `main` advanced from `A` to `G` while `pr-1` and `pr-2` still point at the old stack:

```text
main -- A -- G
         \
pr-1      B -- C
                \
pr-2             D -- E
```

Run:

```sh
git cascade sync
```

Result:

```text
main -- A -- G
              \
pr-1           B' -- C'
                      \
pr-2                   D' -- E'
```

By default, `sync` uses the repository default branch, preferring `origin/HEAD`, then local `main`, then local `master`.
After `sync`, each affected local stack starts from the current tip of the selected target branch.

If this repository targets a non-default integration branch, pass it explicitly:

```sh
git cascade sync --base develop
```

Preview the operation first:

```sh
git cascade sync --dry-run
```

### Branch Was Updated

Use `restack` when a parent branch gained commits and child branches should follow that parent branch.

For example, `pr-1` gained commit `G`:

```text
main -- A
         \
pr-1      B -- C -- G
                \
pr-2             D -- E
                       \
pr-3                    F
```

Move `pr-2` so it starts from the new tip of `pr-1`, and move `pr-3` so it starts from the new tip of `pr-2`:

```sh
git cascade restack pr-1
```

Result:

```text
main -- A
         \
pr-1      B -- C -- G
                     \
pr-2                  D' -- E'
                             \
pr-3                          F'
```

If you are currently on the updated branch, the branch argument can be omitted:

```sh
git switch pr-1
git cascade restack
```

If the stack is based on a non-default integration branch, pass that base explicitly:

```sh
git cascade restack pr-1 --base develop
```

Preview the operation first:

```sh
git cascade restack pr-1 --dry-run
```

### Branch Was Landed

Use `landed` when a parent branch was merged or squashed into a target branch, and child branches should now start from the landed result instead of the old branch.

For example, `pr-1` was squash-merged into `main` as commit `S`:

```text
main -- A -- S
         \
pr-1      B -- C
                \
pr-2             D -- E
                       \
pr-3                    F
```

Move `pr-2` so it starts from the landed result on `main`, and move `pr-3` so it starts from the new tip of `pr-2`:

```sh
git cascade landed pr-1 --onto main
```

Result:

```text
main -- A -- S
              \
pr-2           D' -- E'
                      \
pr-3                   F'
```

When `--onto` is omitted, `git-cascade` uses the default branch, preferring `origin/HEAD`, then local `main`, then local `master`:

For a true merge commit, `landed` uses the merge commit that introduced the old tip. That keeps child branches attached to the landing point instead of accidentally including unrelated later target-branch commits:

```text
main -- A -------- M
         \        /
pr-1      B -- C
                \
pr-2             D -- E
```

Fast-forward landings do not leave enough graph information to tell where the target branch was before the landing. Pass that previous target-branch tip explicitly:

```sh
git cascade landed pr-1 --onto main --old-base <previous-main-tip>
```

Preview the operation first:

```sh
git cascade landed pr-1 --onto main --dry-run
```

### Explicit Replay

Use `replay` when none of the named workflows fit and you can identify the exact old range and replacement tip:

```sh
git cascade replay --old-base main --old-tip pr-1 --new-tip rewritten-pr-1
```

`replay` moves the same kind of dependent branch stack as `sync`, `restack`, and `landed`, but it does not infer the situation for you. You provide the before/after refs directly.

The targeted workflow commands and `replay` default to `move-to-current-tips`, so each child branch moves to its parent's rewritten apply-time tip. All of them expose `--base-strategy` for explicit-control cases.

Preview the generic replay first:

```sh
git cascade replay --old-base main --old-tip pr-1 --new-tip rewritten-pr-1 --dry-run
```

## Underlying Model

`git-cascade` works by creating a repository-local plan and then applying it.

A plan records:

- the old root range, expressed as `old-base..old-tip`
- the local branches that depend on commits in that range
- the parent/child relationships between dependent branches
- each branch's planned commits and fork point

The targeted workflow commands and explicit replay generate plans for you:

- `sync` creates a generated plan under `generated/sync/...`
- `restack` creates a generated plan under `generated/restack/...`
- `landed` creates a generated plan under `generated/landed/...`
- `replay` creates a generated plan under `generated/replay/...`

Generated plans are deleted after a successful apply. If replay stops on a conflict, the generated plan is kept and the active state file points to it, so `git cascade continue` can recover.

By default, generated one-shot commands replay each child branch onto its parent's rewritten apply-time tip. This is the `move-to-current-tips` base strategy. Pass `--base-strategy ...` when you need a different strategy.

## Merge Commits

Dependent branches may contain merge commits, for example after merging the target branch to catch up, or after merging an unrelated local branch. Replay reproduces each merge on the rewritten parents:

- A merge whose merged-in side is already contained in the new base (typically a previous `git merge main`) is redundant and dropped; the branch becomes linear.
- Other merges are recreated with both parents. How their content is reproduced is controlled by `--merge-strategy`:
  - `replay-resolution` (default): the original merge's tree is replayed onto the rewritten first parent, preserving manual conflict resolutions byte-for-byte.
  - `re-merge`: the merge is re-run on the rewritten parents, recomputing conflict resolutions.

Octopus merges (more than two parents) are not supported. Fork points that are ambiguous because of criss-cross merges require an explicit `--old-base`.

## Conflicts

If replay conflicts, permanent branch refs remain unchanged and the active operation state is preserved.

Inspect the active operation:

```sh
git cascade status
```

Resolve conflicts in the reported worktree:

```sh
git -C <worktree> status
git -C <worktree> add <resolved-files>
```

Continue after resolving:

```sh
git cascade continue
```

`continue` also resumes an operation that was interrupted mid-replay, for example by a crash or Ctrl-C; already-finished branches are not replayed again.

Abort and clean the active operation:

```sh
git cascade abort
```

Abort cleans temporary state and leaves the stored plan intact so it can be retried.

## Power-User Plan Commands

Use `git cascade plan ...` when you need to snapshot topology before a destructive rewrite, inspect a plan before applying it, or script the lower-level workflow directly.

Create a named repository-local plan:

```sh
git cascade plan create stack --old-base main --old-tip pr-1
```

For a single-commit root rewrite, use that commit's parent as the explicit old base:

```sh
git cascade plan create stack --old-base '<old-commit>^' --old-tip <old-commit>
```

Inspect stored plans:

```sh
git cascade plan list
git cascade plan show stack
```

Delete a stored plan:

```sh
git cascade plan remove stack
```

Apply a stored plan:

```sh
git cascade plan apply stack --new-tip pr-1
```

Preview a stored plan apply without mutating refs, worktrees, plans, or state:

```sh
git cascade plan apply stack --new-tip pr-1 --dry-run
```

Use `move-to-current-tips` to replay every child onto the parent's rewritten apply-time tip:

```sh
git cascade plan apply stack --new-tip pr-1 --base-strategy move-to-current-tips
```

Use `move-to-planned-tips` when children should move to each parent's rewritten planned tip, ignoring commits added to the parent after planning.

The low-level `plan apply` default base strategy is `preserve-fork-points`:

```sh
git cascade plan apply stack --new-tip pr-1 --base-strategy preserve-fork-points
```

Replay in the current worktree instead of a temporary worktree:

```sh
git cascade plan apply stack --new-tip pr-1 --in-place
```

`--in-place` requires a clean worktree. If replay conflicts, the conflict is left in the current worktree and `git cascade abort` restores the checkout that was active before apply started.

Replace an existing plan:

```sh
git cascade plan create stack --old-base main --old-tip pr-1 --replace
```

## Shell Completions

Generate completion scripts with Clap's built-in shell generators:

```sh
git cascade completions bash
git cascade completions zsh
git cascade completions fish
```

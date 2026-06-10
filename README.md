# git-cascade

Git-native CLI for planning and applying cascade rebases across dependent local branch stacks.

When the `git-cascade` binary is installed on `PATH`, Git exposes it as:

```sh
git cascade <command>
```

Most users should start with the high-level commands:

```sh
git cascade sync
git cascade restack [branch]
git cascade landed <old-tip> [--onto <ref>]
git cascade replay --old-base <ref> --old-tip <ref> --new-tip <ref>
```

The lower-level `git cascade plan ...` commands are for power users who need to inspect, save, or apply plans manually.

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

The common operations are:

- The default branch advanced and local branch stacks should move onto it.
- A branch was updated without rewriting its existing commits.
- A branch landed on `main`, usually as a squash merge or merge commit, and dependent branches need to move to the landed replacement.

### Default Branch Advanced

Use `sync` after pulling `main` when your local branches should move onto the current default branch.

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

By default, `sync` replays onto the current local `main`/`master` if you are on one of those branches, otherwise it uses the default branch. It uses the current `--onto` tip as both the old and new root tip, and infers the old base from the oldest local branch fork point.

If the inferred fork point is not what you want, pass the old range explicitly:

```sh
git cascade sync --onto main --old-tip main --old-base <older-main-commit>
```

Preview the operation first:

```sh
git cascade sync --dry-run
```

### Branch Was Updated

Use `restack` when a parent branch gained commits but its old commits are still present.

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

Move `pr-2` to the new tip of `pr-1`, and move `pr-3` to the new tip of `pr-2`:

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

Preview the operation first:

```sh
git cascade restack pr-1 --dry-run
```

### Branch Was Landed

Use `landed` when a parent branch was merged into `main` and dependent branches should move to the landed replacement on `main`.

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

Move `pr-2` onto `main`, and move `pr-3` onto the new tip of `pr-2`:

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

```sh
git cascade landed pr-1
```

For a true merge commit, `landed` finds the first-parent merge that introduced the old tip and replays dependents onto that merge commit:

```text
main -- A -------- M
         \        /
pr-1      B -- C
                \
pr-2             D -- E
```

```sh
git cascade landed pr-1 --onto main
```

Fast-forward landings do not leave enough graph information to infer the old base after the fact. Pass the previous default-branch tip explicitly:

```sh
git cascade landed pr-1 --onto main --old-base <previous-main-tip>
```

Preview the operation first:

```sh
git cascade landed pr-1 --onto main --dry-run
```

### Generic Replay

Use `replay` when you know the old root and replacement root exactly, but the situation is not simply "same branch advanced" or "branch landed on main":

```sh
git cascade replay --old-base main --old-tip pr-1 --new-tip rewritten-pr-1
```

`replay` generates and stores a temporary plan, applies it, and deletes the plan on success. If replay stops on a conflict, the generated plan is kept so `git cascade continue` can recover.

Like `restack` and `landed`, `replay` defaults to `move-to-current-tips` so each child branch moves to its parent's rewritten apply-time tip.

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

The high-level commands generate plans for you:

- `sync` creates a generated plan under `generated/sync/...`
- `restack` creates a generated plan under `generated/restack/...`
- `landed` creates a generated plan under `generated/landed/...`
- `replay` creates a generated plan under `generated/replay/...`

Generated plans are deleted after a successful apply. If replay stops on a conflict, the generated plan is kept and the active state file points to it, so `git cascade continue` can recover.

By default, one-shot commands replay each child branch onto its parent's rewritten apply-time tip. This is the `move-to-current-tips` strategy.

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
git cascade plan apply stack --new-tip pr-1 --strategy move-to-current-tips
```

Use `move-to-planned-tips` when children should move to each parent's rewritten planned tip, ignoring commits added to the parent after planning.

The low-level `plan apply` default strategy is `preserve-fork-points`:

```sh
git cascade plan apply stack --new-tip pr-1 --strategy preserve-fork-points
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

## Verify

```sh
cargo make ci
```

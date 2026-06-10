# git-cascade

Git-native CLI for planning and applying cascade rebases across dependent local branch stacks.

When the `git-cascade` binary is installed on `PATH`, Git exposes it as:

```sh
git cascade <command>
```

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

The two common operations are:

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

By default, `sync` replays onto the current local `main`/`master` if you are on one of those branches, otherwise it uses the default branch. It uses `<onto>@{1}` as the old default-branch tip, which matches the common "I just pulled main" workflow.

If your branches forked from an older default-branch commit, widen the old range explicitly:

```sh
git cascade sync --onto main --old-tip 'main@{1}' --old-base <older-main-commit>
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

## Explicit Workflow

The one-shot commands above are wrappers around an explicit plan/apply workflow. Use the lower-level commands when you need to inspect, save, or script a plan manually, or when you must snapshot topology before a destructive rewrite.

Create a repository-local plan before rewriting the root range:

```sh
git cascade plan stack --old-base main --old-tip pr-1
```

For a single-commit root rewrite, use that commit's parent as the explicit old base:

```sh
git cascade plan stack --old-base '<old-commit>^' --old-tip <old-commit>
```

Rewrite the replacement root tip manually:

```sh
git switch pr-1
git rebase main
```

After the rewrite, the root branch has moved but the dependents still point at the old commits:

```text
main -- A -- G
              \
pr-1           B' -- C'

old pr-2/pr-3 still point at D/E/F on the old stack
```

At that point, Git no longer has enough branch metadata to know how `pr-2` and `pr-3` related to the old `pr-1` commits. `git-cascade` solves this by recording the dependent branch graph before the manual rewrite, then replaying the dependent branches onto the rewritten commits afterwards.

The result is the same stack shape on the new root:

```text
main -- A -- G
              \
pr-1           B' -- C'
                      \
pr-2                   D' -- E'
                              \
pr-3                           F'
```

Apply the cascade to dependent branches:

```sh
git cascade apply stack --new-tip pr-1
```

Preview the Git commands without mutating refs, worktrees, or state:

```sh
git cascade apply stack --new-tip pr-1 --dry-run
```

Use the simpler strategy that replays every child onto the parent's rewritten apply-time tip:

```sh
git cascade apply stack --new-tip pr-1 --strategy move-to-current-tips
```

Use `move-to-planned-tips` instead when children should move to each parent's rewritten planned tip, ignoring commits added to the parent after planning.

Replay in the current worktree instead of a temporary worktree:

```sh
git cascade apply stack --new-tip pr-1 --in-place
```

`--in-place` requires a clean worktree. If replay conflicts, the conflict is left in the current worktree and `git cascade abort` restores the checkout that was active before apply started.

The default strategy is:

```sh
git cascade apply stack --new-tip pr-1 --strategy preserve-fork-points
```

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

## Plan Management

List stored plans by name:

```sh
git cascade list
```

Show a named plan:

```sh
git cascade show stack
```

Replace an existing plan:

```sh
git cascade plan stack --old-base main --old-tip pr-1 --replace
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

# git-cascade

Git-native CLI for planning and applying cascade rebases across dependent local branch stacks.

When the `git-cascade` binary is installed on `PATH`, Git exposes it as:

```sh
git cascade <command>
```

## Motivation

Branch stacks are easy to build and awkward to rewrite. For example:

```text
main -- A
         \
pr-1      B -- C
                \
pr-2             D -- E
                       \
pr-3                    F
```

If `main` moves, you might manually rebase `pr-1` first:

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

## Workflow

Create a repository-local plan before rewriting the root range:

```sh
git cascade plan stack --old-tip pr-1
```

If inference picks the wrong default/base ref, pass it explicitly:

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
git cascade plan stack --old-tip pr-1 --replace
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

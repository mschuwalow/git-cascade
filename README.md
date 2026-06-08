# git-cascade

Git-native CLI for planning and applying cascade rebases across dependent local branch stacks.

When the `git-cascade` binary is installed on `PATH`, Git exposes it as:

```sh
git cascade <command>
```

## Workflow

Create a repository-local plan before rewriting the anchor branch:

```sh
git cascade plan --anchor pr-1 --name stack
```

Rewrite the anchor branch manually:

```sh
git switch pr-1
git rebase main
```

Apply the cascade to dependent branches:

```sh
git cascade apply --name stack --new-anchor pr-1
```

Preview the Git commands without mutating refs, worktrees, or state:

```sh
git cascade apply --name stack --new-anchor pr-1 --dry-run
```

Use the simpler strategy that replays every child onto the rewritten tip of its parent:

```sh
git cascade apply --name stack --new-anchor pr-1 --strategy move-to-heads
```

The default strategy is:

```sh
git cascade apply --name stack --new-anchor pr-1 --strategy preserve-fork-points
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

## Plan Management

List named plans:

```sh
git cascade list
```

Show a named plan:

```sh
git cascade show --name stack
```

Replace an existing plan:

```sh
git cascade plan --anchor pr-1 --name stack --replace
```

## Current Limits

- Version 1 targets local branches only.
- Version 1 supports linear commit ranges only and rejects merge commits.
- Plans are named and stored under the repository Git common directory.
- Exported or path-based plans are not supported.
- Only one active cascade operation is allowed per repository.

## Verify

```sh
cargo make ci
```

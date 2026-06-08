# git-cascade

Git-native CLI for planning and applying cascade rebases across dependent local branch stacks.

When the `git-cascade` binary is installed on `PATH`, Git exposes it as:

```sh
git cascade <command>
```

## Workflow

Create a repository-local plan before rewriting the root range:

```sh
git cascade plan stack --old-base main --old-tip pr-1
```

For a single-commit root rewrite, use that commit's parent as the old base:

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

Use the simpler strategy that replays every child onto the rewritten tip of its parent:

```sh
git cascade apply stack --new-tip pr-1 --strategy move-to-heads
```

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

## Current Limits

- Version 1 updates dependent local branches only.
- Version 1 supports linear commit ranges only and rejects merge commits.
- Direct dependents are discovered only inside the explicit old root range `old_base..old_tip`.
- Dependent branches may gain new linear commits after planning; apply replays those commits too.
- Apply rejects dependent branches when the planned commit range is no longer reachable from the current branch tip.
- Plans are keyed by explicit names and stored under the repository Git common directory.
- Successful apply removes the stored plan for its anchor.
- Exported or path-based plans are not supported.
- Only one active cascade operation is allowed per repository.

## Verify

```sh
cargo make ci
```

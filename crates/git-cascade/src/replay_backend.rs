use crate::encoding::{decode_component, encode_component};
use crate::git::Git;
use crate::plan::{Node, Plan, PlanCommit};
use crate::state::{
    ApplyState, CurrentState, MergeStrategy, ReplayOp, RestoreState, WorktreeState,
};
use crate::storage::Storage;
use crate::test_hooks;
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

pub(crate) trait ReplayBackend {
    fn start(&mut self, state: &ApplyState) -> Result<()>;
    fn no_branches(&mut self) -> Result<()>;
    fn temp_tips(&mut self, temp_refs: &[String]) -> Result<HashMap<String, String>>;
    fn prepare_branch(
        &mut self,
        state: &ApplyState,
        branch_index: usize,
        total_branches: usize,
        node: &Node,
        base: &str,
    ) -> Result<()>;
    fn start_replay(
        &mut self,
        branch_index: usize,
        total_branches: usize,
        node: &Node,
        total_commits: usize,
        start_commit_index: usize,
        was_resuming: bool,
    ) -> Result<()>;
    /// Moves the replay worktree to `commit` before replaying a commit whose
    /// rewritten first parent is not the current worktree head.
    fn position_worktree(&mut self, state: &ApplyState, commit: &str) -> Result<()>;
    fn cherry_pick(
        &mut self,
        state: &ApplyState,
        node: &Node,
        commit: &str,
        commit_index: usize,
        total_commits: usize,
    ) -> Result<String>;
    /// Whether every mapped non-first parent is already contained in the
    /// mapped first parent, making the merge redundant.
    fn is_redundant_merge(&mut self, mapped_parents: &[String]) -> Result<bool>;
    fn skip_redundant_merge(&mut self, node: &Node, commit: &str, kept: &str) -> Result<()>;
    fn replay_merge(
        &mut self,
        state: &ApplyState,
        node: &Node,
        commit: &PlanCommit,
        mapped_parents: &[String],
        commit_index: usize,
        total_commits: usize,
    ) -> Result<String>;
    fn continue_cherry_pick(
        &mut self,
        state: &ApplyState,
        current: &CurrentState,
    ) -> Result<String>;
    fn continue_merge(&mut self, state: &ApplyState, current: &CurrentState) -> Result<String>;
    fn skip_replay(
        &mut self,
        plan: &Plan,
        node: &Node,
        branch_index: usize,
        total_branches: usize,
        current_tip: &str,
    ) -> Result<(String, String)>;
    fn write_temp_ref(
        &mut self,
        plan: &Plan,
        node: &Node,
        branch_index: usize,
        total_branches: usize,
        rewritten_tip: &str,
    ) -> Result<(String, String)>;
    fn final_update(&mut self, plan: &Plan, state: &ApplyState) -> Result<()>;
    fn delete_applied_plan(&mut self, state: &ApplyState) -> Result<()>;
    fn cleanup_deleting_state(&mut self, state: &mut ApplyState) -> Result<()>;
}

pub(crate) struct GitReplayBackend<'a> {
    git: &'a Git,
    storage: &'a Storage,
}

impl<'a> GitReplayBackend<'a> {
    pub(crate) fn new(git: &'a Git, storage: &'a Storage) -> Self {
        Self { git, storage }
    }
}

impl ReplayBackend for GitReplayBackend<'_> {
    fn start(&mut self, state: &ApplyState) -> Result<()> {
        eprintln!(
            "Applying cascade plan `{}` with base strategy `{}` in {} worktree mode",
            state.plan_name, state.base_strategy, state.worktree
        );
        Ok(())
    }

    fn no_branches(&mut self) -> Result<()> {
        eprintln!("No branches to replay");
        Ok(())
    }

    fn temp_tips(&mut self, temp_refs: &[String]) -> Result<HashMap<String, String>> {
        temp_tips_from_refs(self.git, temp_refs)
    }

    fn prepare_branch(
        &mut self,
        state: &ApplyState,
        branch_index: usize,
        total_branches: usize,
        node: &Node,
        base: &str,
    ) -> Result<()> {
        eprintln!(
            "Preparing branch {branch_index}/{total_branches} `{}`",
            node.branch
        );
        let worktree = std::path::PathBuf::from(state.worktree.path());
        let worktree_git = Git::new(&worktree);
        if state.worktree.is_in_place() && state.completed.temp_refs.is_empty() {
            // A stale cherry-pick or merge can linger after a crashed replay.
            let _ = worktree_git.try_cherry_pick_abort();
            let _ = worktree_git.try_merge_abort();
            worktree_git.switch_detached(base)
        } else if worktree.exists() {
            let _ = worktree_git.try_cherry_pick_abort();
            let _ = worktree_git.try_merge_abort();
            worktree_git.reset_hard(base)
        } else {
            self.git.worktree_add_detached(&worktree, base)
        }
    }

    fn start_replay(
        &mut self,
        branch_index: usize,
        total_branches: usize,
        node: &Node,
        total_commits: usize,
        start_commit_index: usize,
        was_resuming: bool,
    ) -> Result<()> {
        if was_resuming {
            let remaining_commits = total_commits.saturating_sub(start_commit_index);
            eprintln!(
                "Resuming branch {branch_index}/{total_branches} `{}` with {remaining_commits} commit(s) remaining",
                node.branch
            );
        } else {
            eprintln!(
                "Replaying branch {branch_index}/{total_branches} `{}` with {total_commits} commit(s)",
                node.branch
            );
        }
        Ok(())
    }

    fn position_worktree(&mut self, state: &ApplyState, commit: &str) -> Result<()> {
        eprintln!("  reset to {}", short_oid(commit));
        let worktree_git = Git::new(state.worktree.path());
        worktree_git.reset_hard(commit)
    }

    fn cherry_pick(
        &mut self,
        state: &ApplyState,
        node: &Node,
        commit: &str,
        commit_index: usize,
        total_commits: usize,
    ) -> Result<String> {
        eprintln!(
            "  cherry-pick {}/{} {}",
            commit_index + 1,
            total_commits,
            short_oid(commit)
        );
        let worktree = std::path::PathBuf::from(state.worktree.path());
        let worktree_git = Git::new(&worktree);
        if let Err(error) = worktree_git.cherry_pick(commit) {
            if is_empty_cherry_pick(&worktree_git)? {
                worktree_git.cherry_pick_skip()?;
                eprintln!(
                    "  skipped empty commit {}; its changes are already applied",
                    short_oid(commit)
                );
                return worktree_git.head_oid();
            }
            return Err(Error::ApplyStopped {
                branch: node.branch.clone(),
                commit: commit.to_owned(),
                worktree,
                message: error.to_string(),
            });
        }
        worktree_git.head_oid()
    }

    fn is_redundant_merge(&mut self, mapped_parents: &[String]) -> Result<bool> {
        let Some((first, others)) = mapped_parents.split_first() else {
            return Ok(false);
        };
        if others.is_empty() {
            return Ok(false);
        }
        for parent in others {
            if !self.git.is_ancestor(parent, first)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn skip_redundant_merge(&mut self, _node: &Node, commit: &str, kept: &str) -> Result<()> {
        eprintln!(
            "  skipped redundant merge {}; already contained in {}",
            short_oid(commit),
            short_oid(kept)
        );
        Ok(())
    }

    fn replay_merge(
        &mut self,
        state: &ApplyState,
        node: &Node,
        commit: &PlanCommit,
        mapped_parents: &[String],
        commit_index: usize,
        total_commits: usize,
    ) -> Result<String> {
        eprintln!(
            "  merge {}/{} {}",
            commit_index + 1,
            total_commits,
            short_oid(&commit.oid)
        );
        let worktree = std::path::PathBuf::from(state.worktree.path());
        let worktree_git = Git::new(&worktree);
        let author = self.git.commit_author(&commit.oid)?;
        let message = self.git.commit_message(&commit.oid)?;

        match state.merge_strategy {
            MergeStrategy::ReplayResolution => {
                if let Err(error) = worktree_git.cherry_pick_mainline_no_commit(&commit.oid) {
                    return Err(Error::ApplyStopped {
                        branch: node.branch.clone(),
                        commit: commit.oid.clone(),
                        worktree,
                        message: error.to_string(),
                    });
                }
                finish_merge_resolution(&worktree_git, mapped_parents, &message, &author)
            }
            MergeStrategy::ReMerge => {
                let merge_parent = mapped_parents.get(1).ok_or_else(|| {
                    Error::InvalidPlan(format!(
                        "merge commit `{}` has no mapped second parent",
                        commit.oid
                    ))
                })?;
                if let Err(error) = worktree_git.merge_no_ff(merge_parent, &message, &author) {
                    return Err(Error::ApplyStopped {
                        branch: node.branch.clone(),
                        commit: commit.oid.clone(),
                        worktree,
                        message: error.to_string(),
                    });
                }
                worktree_git.head_oid()
            }
        }
    }

    fn skip_replay(
        &mut self,
        plan: &Plan,
        node: &Node,
        branch_index: usize,
        total_branches: usize,
        current_tip: &str,
    ) -> Result<(String, String)> {
        let temp_ref = temp_ref(plan, &node.branch);
        self.git.update_ref(&temp_ref, current_tip)?;
        eprintln!(
            "Branch {branch_index}/{total_branches} `{}` already starts at its replay base; keeping {}",
            node.branch,
            short_oid(current_tip)
        );
        Ok((temp_ref, current_tip.to_owned()))
    }

    fn write_temp_ref(
        &mut self,
        plan: &Plan,
        node: &Node,
        branch_index: usize,
        total_branches: usize,
        rewritten_tip: &str,
    ) -> Result<(String, String)> {
        let temp_ref = temp_ref(plan, &node.branch);
        self.git.update_ref(&temp_ref, rewritten_tip)?;
        eprintln!(
            "Finished branch {branch_index}/{total_branches} `{}` -> {}",
            node.branch,
            short_oid(rewritten_tip)
        );
        Ok((temp_ref, rewritten_tip.to_owned()))
    }

    fn continue_cherry_pick(
        &mut self,
        _state: &ApplyState,
        current: &CurrentState,
    ) -> Result<String> {
        let worktree = std::path::PathBuf::from(&current.worktree);
        let worktree_git = Git::new(&worktree);
        if !worktree_git.unmerged_entries()?.is_empty() {
            return Err(Error::InvalidInvocation(format!(
                "worktree {} still has unresolved conflicts; resolve them and git add the files before continuing",
                worktree.display()
            )));
        }

        if is_empty_cherry_pick(&worktree_git)? {
            worktree_git.cherry_pick_skip()?;
            eprintln!(
                "  skipped empty commit {}; the resolution matches the current tree",
                short_oid(&current.commit)
            );
            return worktree_git.head_oid();
        }

        worktree_git.cherry_pick_continue()?;
        worktree_git.head_oid()
    }

    fn continue_merge(&mut self, _state: &ApplyState, current: &CurrentState) -> Result<String> {
        let worktree = std::path::PathBuf::from(&current.worktree);
        let worktree_git = Git::new(&worktree);
        if !worktree_git.unmerged_entries()?.is_empty() {
            return Err(Error::InvalidInvocation(format!(
                "worktree {} still has unresolved conflicts; resolve them and git add the files before continuing",
                worktree.display()
            )));
        }

        let author = self.git.commit_author(&current.commit)?;
        match current.op {
            ReplayOp::MergeResolution => {
                let message = self.git.commit_message(&current.commit)?;
                finish_merge_resolution(&worktree_git, &current.mapped_parents, &message, &author)
            }
            ReplayOp::ReMerge => {
                worktree_git.commit_no_edit_with_author(&author)?;
                worktree_git.head_oid()
            }
            ReplayOp::CherryPick => Err(Error::InvalidInvocation(
                "continue_merge called for a cherry-pick operation".to_owned(),
            )),
        }
    }

    fn final_update(&mut self, plan: &Plan, state: &ApplyState) -> Result<()> {
        eprintln!("Updating branch refs");
        finish_final_update(self.git, plan, state)
    }

    fn delete_applied_plan(&mut self, state: &ApplyState) -> Result<()> {
        self.storage.delete_plan_if_exists(state.plan_name.clone())
    }

    fn cleanup_deleting_state(&mut self, state: &mut ApplyState) -> Result<()> {
        eprintln!("Cleaning temporary cascade state");
        let worktree = worktree_path(self.storage, state);
        if worktree.exists() {
            let worktree_git = Git::new(&worktree);
            let _ = worktree_git.try_cherry_pick_abort();
            let _ = worktree_git.try_merge_abort();
            if let WorktreeState::InPlace { restore, .. } = &state.worktree {
                match restore {
                    RestoreState::Branch { name, .. } => worktree_git.switch_branch(name)?,
                    RestoreState::Detached { head } => worktree_git.switch_detached(head)?,
                }
            }
        }

        let mut refs = BTreeSet::new();
        refs.extend(state.completed.temp_refs.iter().cloned());
        refs.extend(
            self.git
                .refs_under(&format!("refs/cascade/tmp/{}", state.plan_id))?,
        );
        for temp_ref in refs {
            let _ = self.git.delete_ref(&temp_ref);
        }

        if state.worktree.is_temporary() {
            let _ = self.git.worktree_remove_force(&worktree);
            if worktree.exists() {
                fs::remove_dir_all(&worktree).map_err(|source| Error::IoWithPath {
                    path: worktree.clone(),
                    source,
                })?;
            }
        }

        Ok(())
    }
}

pub(crate) struct DryRunReplayBackend {
    output: String,
    temp_tips: HashMap<String, String>,
}

impl DryRunReplayBackend {
    pub(crate) fn new(
        git: &Git,
        storage: &Storage,
        plan: &Plan,
        state: &ApplyState,
    ) -> Result<Self> {
        let mut output = String::new();
        writeln!(output, "# git-cascade apply --dry-run").unwrap();
        writeln!(output, "new-tip {}", state.new_tip).unwrap();
        writeln!(output, "base-strategy {}", state.base_strategy).unwrap();
        writeln!(output, "merge-strategy {}", state.merge_strategy).unwrap();
        writeln!(output, "worktree-mode {}", state.worktree).unwrap();
        let worktree = if state.worktree.path().is_empty() {
            storage.worktrees_dir().join(plan.plan_id.to_string())
        } else {
            std::path::PathBuf::from(state.worktree.path())
        };
        if state.worktree.is_in_place() && state.worktree.path().is_empty() {
            writeln!(output, "worktree {}", git.worktree_root()?.display()).unwrap();
        } else {
            writeln!(output, "worktree {}", worktree.display()).unwrap();
        }
        writeln!(output, "temp-refs refs/cascade/tmp/{}", plan.plan_id).unwrap();

        Ok(Self {
            output,
            temp_tips: HashMap::new(),
        })
    }

    pub(crate) fn finish(self) -> String {
        self.output
    }
}

impl ReplayBackend for DryRunReplayBackend {
    fn start(&mut self, _state: &ApplyState) -> Result<()> {
        Ok(())
    }

    fn no_branches(&mut self) -> Result<()> {
        Ok(())
    }

    fn temp_tips(&mut self, _temp_refs: &[String]) -> Result<HashMap<String, String>> {
        Ok(self.temp_tips.clone())
    }

    fn prepare_branch(
        &mut self,
        state: &ApplyState,
        _branch_index: usize,
        _total_branches: usize,
        node: &Node,
        base: &str,
    ) -> Result<()> {
        let worktree = std::path::Path::new(state.worktree.path());
        writeln!(self.output).unwrap();
        writeln!(self.output, "# branch {}", node.branch).unwrap();
        writeln!(self.output, "replay-base {base}").unwrap();
        if state.worktree.is_in_place() && state.completed.temp_refs.is_empty() {
            writeln!(
                self.output,
                "git -C {} switch --detach {base}",
                worktree.display()
            )
            .unwrap();
        } else if state.completed.temp_refs.is_empty() && state.worktree.is_temporary() {
            writeln!(
                self.output,
                "git worktree add --detach {} {base}",
                worktree.display()
            )
            .unwrap();
        } else {
            writeln!(
                self.output,
                "git -C {} reset --hard {base}",
                worktree.display()
            )
            .unwrap();
        }
        Ok(())
    }

    fn start_replay(
        &mut self,
        _branch_index: usize,
        _total_branches: usize,
        _node: &Node,
        _total_commits: usize,
        _start_commit_index: usize,
        _was_resuming: bool,
    ) -> Result<()> {
        Ok(())
    }

    fn position_worktree(&mut self, state: &ApplyState, commit: &str) -> Result<()> {
        writeln!(
            self.output,
            "git -C {} reset --hard {commit}",
            state.worktree.path()
        )
        .unwrap();
        Ok(())
    }

    fn cherry_pick(
        &mut self,
        state: &ApplyState,
        node: &Node,
        commit: &str,
        _commit_index: usize,
        _total_commits: usize,
    ) -> Result<String> {
        writeln!(
            self.output,
            "git -C {} cherry-pick {commit}",
            state.worktree.path()
        )
        .unwrap();
        if commit == node.tip {
            Ok(format!("<rewritten {} planned tip>", node.branch))
        } else {
            Ok(format!("<rewritten {}:{commit}>", node.branch))
        }
    }

    fn is_redundant_merge(&mut self, _mapped_parents: &[String]) -> Result<bool> {
        // Rewritten commits do not exist during a dry run; always print the
        // merge replay with an annotation instead.
        Ok(false)
    }

    fn skip_redundant_merge(&mut self, _node: &Node, commit: &str, kept: &str) -> Result<()> {
        writeln!(
            self.output,
            "# merge {commit} dropped; already contained in {kept}"
        )
        .unwrap();
        Ok(())
    }

    fn replay_merge(
        &mut self,
        state: &ApplyState,
        node: &Node,
        commit: &PlanCommit,
        mapped_parents: &[String],
        _commit_index: usize,
        _total_commits: usize,
    ) -> Result<String> {
        let worktree = state.worktree.path();
        writeln!(
            self.output,
            "# merge {} may be dropped at apply time if already contained in the new base",
            commit.oid
        )
        .unwrap();
        match state.merge_strategy {
            MergeStrategy::ReplayResolution => {
                writeln!(
                    self.output,
                    "git -C {worktree} cherry-pick -m 1 --no-commit {}",
                    commit.oid
                )
                .unwrap();
                let parent_args = mapped_parents
                    .iter()
                    .map(|parent| format!("-p {parent}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                writeln!(
                    self.output,
                    "git -C {worktree} commit-tree <tree> {parent_args}"
                )
                .unwrap();
            }
            MergeStrategy::ReMerge => {
                let merge_parent = mapped_parents
                    .get(1)
                    .map(String::as_str)
                    .unwrap_or("<missing parent>");
                writeln!(
                    self.output,
                    "git -C {worktree} merge --no-ff {merge_parent}"
                )
                .unwrap();
            }
        }

        if commit.oid == node.tip {
            Ok(format!("<rewritten {} planned tip>", node.branch))
        } else {
            Ok(format!("<rewritten {}:{}>", node.branch, commit.oid))
        }
    }

    fn skip_replay(
        &mut self,
        plan: &Plan,
        node: &Node,
        _branch_index: usize,
        _total_branches: usize,
        current_tip: &str,
    ) -> Result<(String, String)> {
        let temp_ref = temp_ref(plan, &node.branch);
        writeln!(self.output).unwrap();
        writeln!(self.output, "# branch {}", node.branch).unwrap();
        writeln!(
            self.output,
            "already starts at its replay base; keeping {current_tip}"
        )
        .unwrap();
        writeln!(self.output, "git update-ref {temp_ref} {current_tip}").unwrap();
        self.temp_tips
            .insert(node.branch.clone(), current_tip.to_owned());
        Ok((temp_ref, current_tip.to_owned()))
    }

    fn write_temp_ref(
        &mut self,
        plan: &Plan,
        node: &Node,
        _branch_index: usize,
        _total_branches: usize,
        _rewritten_tip: &str,
    ) -> Result<(String, String)> {
        let temp_ref = temp_ref(plan, &node.branch);
        let rewritten_tip = format!("<rewritten {} tip>", node.branch);
        let current_tip = format!("<rewritten {} current tip>", node.branch);
        writeln!(self.output, "git update-ref {temp_ref} HEAD").unwrap();
        self.temp_tips.insert(node.branch.clone(), rewritten_tip);
        Ok((temp_ref, current_tip))
    }

    fn continue_cherry_pick(
        &mut self,
        _state: &ApplyState,
        current: &CurrentState,
    ) -> Result<String> {
        Ok(format!("<rewritten {}:{}>", current.branch, current.commit))
    }

    fn continue_merge(&mut self, _state: &ApplyState, current: &CurrentState) -> Result<String> {
        Ok(format!("<rewritten {}:{}>", current.branch, current.commit))
    }

    fn final_update(&mut self, plan: &Plan, state: &ApplyState) -> Result<()> {
        writeln!(self.output).unwrap();
        writeln!(self.output, "# final ref transaction").unwrap();
        writeln!(self.output, "git update-ref --stdin <<'EOF'").unwrap();
        self.output.push_str(&final_ref_transaction(
            plan,
            &self.temp_tips,
            &state.branch_tips,
        )?);
        writeln!(self.output, "EOF").unwrap();
        Ok(())
    }

    fn delete_applied_plan(&mut self, _state: &ApplyState) -> Result<()> {
        Ok(())
    }

    fn cleanup_deleting_state(&mut self, state: &mut ApplyState) -> Result<()> {
        let WorktreeState::InPlace { path, restore } = &state.worktree else {
            return Ok(());
        };

        writeln!(self.output).unwrap();
        writeln!(self.output, "# restore checkout").unwrap();
        match restore {
            RestoreState::Branch { name, .. } => {
                writeln!(self.output, "git -C {} switch {name}", path).unwrap();
            }
            RestoreState::Detached { head } => {
                writeln!(self.output, "git -C {} switch --detach {head}", path).unwrap();
            }
        }
        Ok(())
    }
}

/// Detects a cherry-pick that stopped because it became empty: the pick is
/// still in progress, but there is nothing to resolve and nothing staged.
fn is_empty_cherry_pick(worktree_git: &Git) -> Result<bool> {
    Ok(worktree_git.cherry_pick_in_progress()?
        && worktree_git.unmerged_entries()?.is_empty()
        && !worktree_git.has_staged_changes()?)
}

/// Commits the staged merge result with the mapped parents, preserving the
/// original message and author.
fn finish_merge_resolution(
    worktree_git: &Git,
    mapped_parents: &[String],
    message: &str,
    author: &crate::git::CommitAuthor,
) -> Result<String> {
    let tree = worktree_git.write_tree()?;
    let merge_commit = worktree_git.commit_tree(&tree, mapped_parents, message, author)?;
    let _ = worktree_git.try_cherry_pick_quit();
    worktree_git.reset_hard(&merge_commit)?;
    Ok(merge_commit)
}

fn finish_final_update(git: &Git, plan: &Plan, state: &ApplyState) -> Result<()> {
    let branches = plan
        .nodes
        .iter()
        .map(|node| node.branch.clone())
        .collect::<Vec<_>>();
    let temp_tips = temp_tips_from_refs(git, &state.completed.temp_refs)?;
    ensure_target_branches_not_checked_out(git, &branches)?;
    if final_update_already_applied(git, plan, &temp_tips, &state.branch_tips)? {
        return Ok(());
    }
    test_hooks::run("before-final-update")?;
    git.update_ref_transaction(&final_ref_transaction(
        plan,
        &temp_tips,
        &state.branch_tips,
    )?)
}

fn final_update_already_applied(
    git: &Git,
    plan: &Plan,
    temp_tips: &HashMap<String, String>,
    branch_tips: &BTreeMap<String, String>,
) -> Result<bool> {
    let mut saw_updated = false;
    let mut saw_pending = false;
    for node in &plan.nodes {
        let rewritten_tip = temp_tips.get(&node.branch).ok_or_else(|| {
            Error::InvalidPlan(format!("branch `{}` has no rewritten tip", node.branch))
        })?;
        let expected_tip = branch_tips.get(&node.branch).ok_or_else(|| {
            Error::InvalidPlan(format!("branch `{}` has no expected tip", node.branch))
        })?;
        let current_tip = git.local_branch_tip(&node.branch)?;
        if &current_tip == expected_tip {
            if expected_tip != rewritten_tip {
                saw_pending = true;
            }
            continue;
        }
        if &current_tip == rewritten_tip {
            saw_updated = true;
            continue;
        }

        return Err(Error::InvalidInvocation(format!(
            "branch `{}` moved after apply started: expected `{}` or rewritten tip `{}`, found `{current_tip}`",
            node.branch, expected_tip, rewritten_tip
        )));
    }

    if saw_updated && saw_pending {
        return Err(Error::InvalidInvocation(
            "final update appears partially applied; refusing to continue automatically".to_owned(),
        ));
    }

    Ok(saw_updated && !saw_pending)
}

fn temp_tips_from_refs(git: &Git, temp_refs: &[String]) -> Result<HashMap<String, String>> {
    let mut temp_tips = HashMap::new();
    for temp_ref in temp_refs {
        let Some(encoded_branch) = temp_ref.rsplit('/').next() else {
            continue;
        };
        let branch = decode_component(encoded_branch)?;
        temp_tips.insert(branch, git.resolve_commit(temp_ref)?);
    }

    Ok(temp_tips)
}

fn temp_ref(plan: &Plan, branch: &str) -> String {
    format!(
        "refs/cascade/tmp/{}/{}",
        plan.plan_id,
        encode_component(branch)
    )
}

fn final_ref_transaction(
    plan: &Plan,
    temp_tips: &HashMap<String, String>,
    branch_tips: &BTreeMap<String, String>,
) -> Result<String> {
    let mut transaction = String::new();
    writeln!(transaction, "start").unwrap();
    for node in &plan.nodes {
        let new_tip = temp_tips.get(&node.branch).ok_or_else(|| {
            Error::InvalidPlan(format!("branch `{}` has no rewritten tip", node.branch))
        })?;
        let expected_tip = branch_tips.get(&node.branch).ok_or_else(|| {
            Error::InvalidPlan(format!("branch `{}` has no expected tip", node.branch))
        })?;
        writeln!(
            transaction,
            "update refs/heads/{} {} {}",
            node.branch, new_tip, expected_tip
        )
        .unwrap();
    }
    writeln!(transaction, "prepare").unwrap();
    writeln!(transaction, "commit").unwrap();

    Ok(transaction)
}

fn ensure_target_branches_not_checked_out(git: &Git, branches: &[String]) -> Result<()> {
    let checked_out = git.checked_out_branches()?;
    ensure_branches_not_checked_out(branches, &checked_out)
}

fn ensure_branches_not_checked_out(branches: &[String], checked_out: &[String]) -> Result<()> {
    let blocked = branches
        .iter()
        .filter(|branch| checked_out.contains(branch))
        .cloned()
        .collect::<Vec<_>>();
    if blocked.is_empty() {
        return Ok(());
    }

    Err(Error::InvalidInvocation(format!(
        "cannot apply while target branch(es) are checked out in a worktree: {}. Switch those worktrees to another branch or a detached HEAD before running apply.",
        blocked.join(", ")
    )))
}

fn worktree_path(storage: &Storage, state: &ApplyState) -> PathBuf {
    if state.worktree.path().is_empty() {
        storage.worktrees_dir().join(state.plan_id.to_string())
    } else {
        PathBuf::from(state.worktree.path())
    }
}

fn short_oid(oid: &str) -> &str {
    oid.get(..12).unwrap_or(oid)
}

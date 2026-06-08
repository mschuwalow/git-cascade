use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use crate::encoding::{decode_component, encode_component};
use crate::git::Git;
use crate::plan::{Node, Plan};
use crate::plan_validate::topological_order;
use crate::state::{ApplyState, CurrentState, RestoreState, WorktreeState};
use crate::storage::Storage;
use crate::test_hooks;
use crate::{Error, Result};

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
    fn cherry_pick(
        &mut self,
        state: &ApplyState,
        node: &Node,
        commit: &str,
        commit_index: usize,
        total_commits: usize,
    ) -> Result<String>;
    fn continue_cherry_pick(
        &mut self,
        state: &ApplyState,
        current: &CurrentState,
    ) -> Result<String>;
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

pub(crate) struct DryRunReplayBackend {
    output: String,
    temp_tips: HashMap<String, String>,
}

impl<'a> GitReplayBackend<'a> {
    pub(crate) fn new(git: &'a Git, storage: &'a Storage) -> Self {
        Self { git, storage }
    }
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
        writeln!(
            output,
            "new-tip {} -> {}",
            state.new_tip.input, state.new_tip.resolved
        )
        .unwrap();
        writeln!(output, "strategy {}", state.strategy).unwrap();
        writeln!(output, "worktree-mode {}", state.worktree).unwrap();
        let worktree = if state.worktree.path().is_empty() {
            storage.worktrees_dir().join(&plan.plan_id)
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

impl ReplayBackend for GitReplayBackend<'_> {
    fn start(&mut self, state: &ApplyState) -> Result<()> {
        eprintln!(
            "Applying cascade plan `{}` with strategy `{}` in {} worktree mode",
            state.plan_name, state.strategy, state.worktree
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
            worktree_git.switch_detached(base)
        } else if worktree.exists() {
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
            return Err(Error::ApplyStopped {
                branch: node.branch.clone(),
                commit: commit.to_owned(),
                worktree,
                message: error.to_string(),
            });
        }
        worktree_git.head_oid()
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

        worktree_git.cherry_pick_continue()?;
        worktree_git.head_oid()
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
        if commit == node.old_tip {
            Ok(format!("<rewritten {} planned tip>", node.branch))
        } else {
            Ok(format!("<rewritten {}:{commit}>", node.branch))
        }
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

    fn final_update(&mut self, plan: &Plan, state: &ApplyState) -> Result<()> {
        let ordered = topological_order(plan)?;
        let nodes = plan
            .nodes
            .iter()
            .map(|node| (node.branch.as_str(), node))
            .collect::<HashMap<_, _>>();
        writeln!(self.output).unwrap();
        writeln!(self.output, "# final ref transaction").unwrap();
        writeln!(self.output, "git update-ref --stdin <<'EOF'").unwrap();
        self.output.push_str(&final_ref_transaction(
            &ordered,
            &nodes,
            &self.temp_tips,
            &state.branch_tips,
            None,
            &state.new_tip.resolved,
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

fn finish_final_update(git: &Git, plan: &Plan, state: &ApplyState) -> Result<()> {
    let ordered = topological_order(plan)?;
    let nodes = plan
        .nodes
        .iter()
        .map(|node| (node.branch.as_str(), node))
        .collect::<HashMap<_, _>>();
    let temp_tips = temp_tips_from_refs(git, &state.completed.temp_refs)?;
    ensure_target_branches_not_checked_out(git, &ordered)?;
    test_hooks::run("before-final-update")?;
    git.update_ref_transaction(&final_ref_transaction(
        &ordered,
        &nodes,
        &temp_tips,
        &state.branch_tips,
        None,
        &state.new_tip.resolved,
    )?)
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
    ordered: &[String],
    nodes: &HashMap<&str, &Node>,
    temp_tips: &HashMap<String, String>,
    branch_tips: &BTreeMap<String, String>,
    new_tip_ref: Option<&str>,
    new_tip: &str,
) -> Result<String> {
    let mut transaction = String::new();
    writeln!(transaction, "start").unwrap();
    if let Some(new_tip_ref) = new_tip_ref {
        writeln!(transaction, "verify {new_tip_ref} {new_tip}").unwrap();
    }
    for branch in ordered {
        let node = nodes
            .get(branch.as_str())
            .ok_or_else(|| Error::InvalidPlan(format!("unknown node `{branch}` in order")))?;
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
        storage.worktrees_dir().join(&state.plan_id)
    } else {
        PathBuf::from(state.worktree.path())
    }
}

fn short_oid(oid: &str) -> &str {
    oid.get(..12).unwrap_or(oid)
}

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;

use crate::encoding::{decode_component, encode_component};
use crate::git::Git;
use crate::plan::{Node, Plan};
use crate::plan_validate::{topological_order, validate_plan_for_apply};
use crate::recovery;
use crate::state::{
    ApplyState, ApplyStateInput, CurrentState, Phase, StateFile, Strategy, initial_apply_state,
};
use crate::storage::{PlanName, Storage};
use crate::test_hooks;
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct DryRunOptions {
    pub new_tip_input: String,
    pub strategy: Strategy,
}

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    pub plan_name: PlanName,
    pub new_tip_input: String,
    pub strategy: Strategy,
}

#[derive(Debug, Clone)]
enum ReplayBase {
    ResolvedCommit(String),
    RewrittenCommit { branch: String, old_commit: String },
    RewrittenPlannedTip { branch: String },
    RewrittenCurrentTip { branch: String },
}

struct ActualReplayContext<'a> {
    nodes: &'a HashMap<&'a str, &'a Node>,
    selected_bases: &'a HashMap<String, String>,
    temp_tips: &'a HashMap<String, String>,
    mappings: &'a BTreeMap<String, String>,
    new_tip: &'a str,
    strategy: Strategy,
}

struct ReplayProgress<'a> {
    state_file: &'a mut StateFile,
    state: &'a mut ApplyState,
    mappings: &'a mut BTreeMap<String, String>,
    selected_bases: &'a mut HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct BranchReplay {
    expected_tip: String,
    extra_commits: Vec<String>,
}

impl ReplayBase {
    fn display(&self) -> String {
        match self {
            Self::ResolvedCommit(commit) => commit.clone(),
            Self::RewrittenCommit { branch, old_commit } => {
                format!("<rewritten {branch}:{old_commit}>")
            }
            Self::RewrittenPlannedTip { branch } => {
                format!("<rewritten {branch} planned tip>")
            }
            Self::RewrittenCurrentTip { branch } => {
                format!("<rewritten {branch} current tip>")
            }
        }
    }
}

pub fn dry_run(
    git: &Git,
    storage: &Storage,
    plan: &Plan,
    options: DryRunOptions,
) -> Result<String> {
    validate_plan_for_apply(git, plan)?;
    let new_tip = git.resolve_commit(&options.new_tip_input)?;
    let ordered = topological_order(plan)?;
    let branch_replays = branch_replays_for_apply(git, plan, &ordered)?;
    let nodes = plan
        .nodes
        .iter()
        .map(|node| (node.branch.as_str(), node))
        .collect::<HashMap<_, _>>();
    let mut selected_bases = HashMap::<String, ReplayBase>::new();
    let mut output = String::new();
    let worktree = storage.worktrees_dir().join(&plan.plan_id);
    let strategy = options.strategy.as_str();

    writeln!(output, "# git-cascade apply --dry-run").unwrap();
    writeln!(output, "new-tip {} -> {}", options.new_tip_input, new_tip).unwrap();
    writeln!(output, "strategy {strategy}").unwrap();
    writeln!(output, "worktree {}", worktree.display()).unwrap();
    writeln!(output, "temp-refs refs/cascade/tmp/{}", plan.plan_id).unwrap();

    for (index, branch) in ordered.iter().enumerate() {
        let node = nodes
            .get(branch.as_str())
            .ok_or_else(|| Error::InvalidPlan(format!("unknown node `{branch}` in order")))?;
        let base = replay_base(node, &nodes, &selected_bases, &new_tip, options.strategy)?;
        selected_bases.insert(node.branch.clone(), base.clone());

        writeln!(output).unwrap();
        writeln!(output, "# branch {}", node.branch).unwrap();
        writeln!(output, "replay-base {}", base.display()).unwrap();
        if index == 0 {
            writeln!(
                output,
                "git worktree add --detach {} {}",
                worktree.display(),
                base.display()
            )
            .unwrap();
        } else {
            writeln!(
                output,
                "git -C {} reset --hard {}",
                worktree.display(),
                base.display()
            )
            .unwrap();
        }
        for commit in replay_commits(node, &branch_replays) {
            writeln!(output, "git -C {} cherry-pick {commit}", worktree.display()).unwrap();
        }
        writeln!(
            output,
            "git update-ref refs/cascade/tmp/{}/{} HEAD",
            plan.plan_id,
            encode_component(&node.branch)
        )
        .unwrap();
    }

    writeln!(output).unwrap();
    writeln!(output, "# final ref transaction").unwrap();
    writeln!(output, "git update-ref --stdin <<'EOF'").unwrap();
    writeln!(output, "start").unwrap();
    for branch in &ordered {
        let node = nodes
            .get(branch.as_str())
            .ok_or_else(|| Error::InvalidPlan(format!("unknown node `{branch}` in order")))?;
        writeln!(
            output,
            "update refs/heads/{} <rewritten {} tip> {}",
            node.branch, node.branch, branch_replays[&node.branch].expected_tip
        )
        .unwrap();
    }
    writeln!(output, "prepare").unwrap();
    writeln!(output, "commit").unwrap();
    writeln!(output, "EOF").unwrap();

    Ok(output)
}

pub fn execute(git: &Git, storage: &Storage, plan: &Plan, options: ApplyOptions) -> Result<()> {
    validate_plan_for_apply(git, plan)?;
    let new_tip = git.resolve_commit(&options.new_tip_input)?;
    let new_tip_input_was_ref = git.symbolic_full_name(&options.new_tip_input)?.is_some();
    let ordered = topological_order(plan)?;
    ensure_target_branches_not_checked_out(git, &ordered)?;
    let branch_replays = branch_replays_for_apply(git, plan, &ordered)?;
    let branch_tips = branch_replays
        .iter()
        .map(|(branch, replay)| (branch.clone(), replay.expected_tip.clone()))
        .collect::<BTreeMap<_, _>>();
    let extra_commits = branch_replays
        .iter()
        .map(|(branch, replay)| (branch.clone(), replay.extra_commits.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut mappings = BTreeMap::new();
    mappings.insert(plan.source.old_tip.clone(), new_tip.clone());
    let worktree = storage.worktrees_dir().join(&plan.plan_id);
    let state = initial_apply_state(ApplyStateInput {
        plan_name: &options.plan_name,
        plan_id: &plan.plan_id,
        new_tip_input: &options.new_tip_input,
        new_tip_resolved: &new_tip,
        new_tip_input_was_ref,
        strategy: options.strategy,
        pending_branches: ordered.clone(),
        branch_tips: branch_tips.clone(),
        extra_commits: extra_commits.clone(),
        mappings: mappings.clone(),
        worktree: worktree.display().to_string(),
    })?;
    let state_file = StateFile::create(storage, &state)?;

    storage.ensure_worktrees_dir()?;
    cleanup_stale_worktree(git, &worktree)?;
    run_apply_state(git, storage, plan, state_file, state)
}

pub fn continue_apply(git: &Git, storage: &Storage) -> Result<()> {
    let mut state_file = StateFile::open(storage)?
        .ok_or_else(|| Error::InvalidInvocation("no active cascade operation".to_owned()))?;
    let mut state = state_file.read_state()?;
    if state.phase == Phase::Deleting {
        recovery::cleanup_state_artifacts(git, storage, state_file, &state)?;
        return Err(Error::InvalidInvocation(
            "no active cascade operation".to_owned(),
        ));
    }
    if state.phase != Phase::Conflict {
        return Err(Error::InvalidInvocation(format!(
            "cannot continue cascade operation in phase `{}`",
            state.phase
        )));
    }

    let current = state.current.clone().ok_or_else(|| {
        Error::InvalidInvocation("active apply state has no current commit".to_owned())
    })?;
    let plan_name = state.plan_name.clone();
    let plan: Plan = serde_yaml::from_str(&storage.read_plan(&plan_name)?)?;
    validate_plan_for_apply(git, &plan)?;

    if state.new_tip.input_was_ref {
        let resolved = git.resolve_commit(&state.new_tip.input)?;
        if resolved != state.new_tip.resolved {
            return Err(Error::InvalidInvocation(format!(
                "new tip `{}` moved after apply started: expected `{}`, found `{resolved}`",
                state.new_tip.input, state.new_tip.resolved
            )));
        }
    }

    let worktree = std::path::PathBuf::from(&current.worktree);
    let worktree_git = Git::new(&worktree);
    if !worktree_git.unmerged_entries()?.is_empty() {
        return Err(Error::InvalidInvocation(format!(
            "worktree {} still has unresolved conflicts; resolve them and git add the files before continuing",
            worktree.display()
        )));
    }

    worktree_git.cherry_pick_continue()?;
    state
        .mappings
        .insert(current.commit.clone(), worktree_git.head_oid()?);
    state.phase = Phase::Replay;
    state_file.write_state(&mut state)?;

    run_apply_state(git, storage, &plan, state_file, state)
}

fn run_apply_state(
    git: &Git,
    storage: &Storage,
    plan: &Plan,
    mut state_file: StateFile,
    mut state: ApplyState,
) -> Result<()> {
    loop {
        match state.phase {
            Phase::Replay => {
                replay_pending_branches(git, plan, &mut state_file, &mut state)?;
                state.phase = Phase::FinalUpdate;
                state.current = None;
                state_file.write_state(&mut state)?;
            }
            Phase::FinalUpdate => {
                finish_final_update(git, plan, &state)?;
                let plan_name = state.plan_name.clone();
                recovery::mark_deleting_and_cleanup(git, storage, state_file, &mut state)?;
                storage.delete_plan(plan_name)?;
                return Ok(());
            }
            Phase::Conflict => {
                return Err(Error::InvalidInvocation(
                    "cannot advance cascade operation with unresolved conflicts; resolve them and run git cascade continue".to_owned(),
                ));
            }
            Phase::Deleting => {
                recovery::cleanup_state_artifacts(git, storage, state_file, &state)?;
                return Ok(());
            }
        }
    }
}

fn replay_pending_branches(
    git: &Git,
    plan: &Plan,
    state_file: &mut StateFile,
    state: &mut ApplyState,
) -> Result<()> {
    let nodes = plan
        .nodes
        .iter()
        .map(|node| (node.branch.as_str(), node))
        .collect::<HashMap<_, _>>();
    let worktree = std::path::PathBuf::from(&state.worktree);
    let worktree_git = Git::new(&worktree);
    let mut mappings = state.mappings.clone();
    let mut temp_refs = state.completed.temp_refs.clone();
    let mut temp_tips = temp_tips_from_refs(git, &temp_refs)?;
    let mut selected_bases = selected_bases_from_mappings(plan, &mappings);

    while let Some(branch) = state.pending.branches.first().cloned() {
        let node = nodes
            .get(branch.as_str())
            .ok_or_else(|| Error::InvalidPlan(format!("unknown pending branch `{branch}`")))?;
        let mut progress = ReplayProgress {
            state_file,
            state,
            mappings: &mut mappings,
            selected_bases: &mut selected_bases,
        };
        let start_commit_index = prepare_branch_replay(
            git,
            &worktree_git,
            &worktree,
            node,
            &nodes,
            &temp_tips,
            &mut progress,
        )?;

        let commits = replay_commits_from_extra(node, &state.extra_commits);
        for commit in commits.iter().skip(start_commit_index) {
            state.phase = Phase::Replay;
            state.current = Some(CurrentState {
                branch: node.branch.clone(),
                commit: commit.clone(),
                worktree: worktree.display().to_string(),
            });
            state_file.write_state(state)?;

            if let Err(error) = worktree_git.cherry_pick(commit) {
                state.phase = Phase::Conflict;
                state_file.write_state(state)?;
                return Err(Error::ApplyStopped {
                    branch: node.branch.clone(),
                    commit: commit.clone(),
                    worktree: worktree.clone(),
                    message: error.to_string(),
                });
            }
            mappings.insert(commit.clone(), worktree_git.head_oid()?);
            state.mappings = mappings.clone();
            state_file.write_state(state)?;
        }

        let rewritten_tip = worktree_git.head_oid()?;
        let temp_ref = temp_ref(plan, &node.branch);
        git.update_ref(&temp_ref, &rewritten_tip)?;
        temp_tips.insert(node.branch.clone(), rewritten_tip);
        if !temp_refs.contains(&temp_ref) {
            temp_refs.push(temp_ref);
        }
        state.completed.temp_refs = temp_refs.clone();
        state.current = None;
        remove_pending_branch(state, &branch)?;
        state_file.write_state(state)?;
    }

    Ok(())
}

fn prepare_branch_replay(
    git: &Git,
    worktree_git: &Git,
    worktree: &std::path::Path,
    node: &Node,
    nodes: &HashMap<&str, &Node>,
    temp_tips: &HashMap<String, String>,
    progress: &mut ReplayProgress<'_>,
) -> Result<usize> {
    if let Some(current) = &progress.state.current {
        if current.branch != node.branch {
            return Err(Error::InvalidPlan(format!(
                "current branch `{}` is not the next pending branch `{}`",
                current.branch, node.branch
            )));
        }
        if !progress.mappings.contains_key(&current.commit) {
            return Err(Error::InvalidPlan(format!(
                "current commit `{}` for branch `{}` has no rewritten mapping",
                current.commit, current.branch
            )));
        }

        return replay_commits_from_extra(node, &progress.state.extra_commits)
            .iter()
            .position(|commit| commit == &current.commit)
            .map(|index| index + 1)
            .ok_or_else(|| {
                Error::InvalidPlan(format!(
                    "current commit `{}` is not part of branch `{}`",
                    current.commit, current.branch
                ))
            });
    }

    let base = actual_replay_base(
        node,
        ActualReplayContext {
            nodes,
            selected_bases: progress.selected_bases,
            temp_tips,
            mappings: progress.mappings,
            new_tip: &progress.state.new_tip.resolved,
            strategy: progress.state.strategy,
        },
    )?;
    progress
        .selected_bases
        .insert(node.branch.clone(), base.clone());
    progress.mappings.insert(
        node.old_base().expect("node has old base").to_owned(),
        base.clone(),
    );
    progress.state.mappings = progress.mappings.clone();
    progress.state.phase = Phase::Replay;
    progress.state_file.write_state(progress.state)?;

    if worktree.exists() {
        worktree_git.reset_hard(&base)?;
    } else {
        git.worktree_add_detached(worktree, &base)?;
    }

    Ok(0)
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
    let new_tip_ref = new_tip_ref_for_state(git, state)?;
    test_hooks::run("before-final-update")?;
    git.update_ref_transaction(&final_ref_transaction(
        &ordered,
        &nodes,
        &temp_tips,
        &state.branch_tips,
        new_tip_ref.as_deref(),
        &state.new_tip.resolved,
    )?)
}

fn new_tip_ref_for_state(git: &Git, state: &ApplyState) -> Result<Option<String>> {
    if !state.new_tip.input_was_ref {
        return Ok(None);
    }

    let resolved = git.resolve_commit(&state.new_tip.input)?;
    if resolved != state.new_tip.resolved {
        return Err(Error::InvalidInvocation(format!(
            "new tip `{}` moved after apply started: expected `{}`, found `{resolved}`",
            state.new_tip.input, state.new_tip.resolved
        )));
    }

    git.symbolic_full_name(&state.new_tip.input)?.map_or_else(
        || {
            Err(Error::InvalidInvocation(format!(
                "new tip `{}` no longer resolves to a ref",
                state.new_tip.input
            )))
        },
        |name| Ok(Some(name)),
    )
}

fn remove_pending_branch(state: &mut ApplyState, branch: &str) -> Result<()> {
    if state.pending.branches.first().map(String::as_str) != Some(branch) {
        return Err(Error::InvalidPlan(format!(
            "completed branch `{branch}` is not first in pending state"
        )));
    }
    state.pending.branches.remove(0);
    Ok(())
}

fn replay_base(
    node: &Node,
    nodes: &HashMap<&str, &Node>,
    selected_bases: &HashMap<String, ReplayBase>,
    new_tip: &str,
    strategy: Strategy,
) -> Result<ReplayBase> {
    if node.is_root() {
        return Ok(ReplayBase::ResolvedCommit(new_tip.to_owned()));
    }

    let parent_branch = node.parent().ok_or_else(|| {
        Error::InvalidPlan(format!("root node `{}` has no branch parent", node.branch))
    })?;
    let parent = nodes
        .get(parent_branch)
        .ok_or_else(|| Error::InvalidPlan(format!("unknown parent `{parent_branch}`")))?;

    if strategy == Strategy::MoveToPlannedTips {
        return Ok(ReplayBase::RewrittenPlannedTip {
            branch: parent.branch.clone(),
        });
    }

    if strategy == Strategy::MoveToCurrentTips {
        return Ok(ReplayBase::RewrittenCurrentTip {
            branch: parent.branch.clone(),
        });
    }

    let old_base = node.old_base().expect("dependent node has old base");
    if Some(old_base) == parent.old_base() {
        return selected_bases.get(&parent.branch).cloned().ok_or_else(|| {
            Error::InvalidPlan(format!(
                "parent `{}` has no selected replay base",
                parent.branch
            ))
        });
    }

    Ok(ReplayBase::RewrittenCommit {
        branch: parent.branch.clone(),
        old_commit: old_base.to_owned(),
    })
}

fn actual_replay_base(node: &Node, context: ActualReplayContext<'_>) -> Result<String> {
    if node.is_root() {
        return Ok(context.new_tip.to_owned());
    }

    let parent_branch = node.parent().ok_or_else(|| {
        Error::InvalidPlan(format!("root node `{}` has no branch parent", node.branch))
    })?;
    let parent = context
        .nodes
        .get(parent_branch)
        .ok_or_else(|| Error::InvalidPlan(format!("unknown parent `{parent_branch}`")))?;

    if context.strategy == Strategy::MoveToPlannedTips {
        return context
            .mappings
            .get(&parent.old_tip)
            .cloned()
            .ok_or_else(|| {
                Error::InvalidPlan(format!(
                    "parent `{}` has no rewritten planned tip",
                    parent.branch
                ))
            });
    }

    if context.strategy == Strategy::MoveToCurrentTips {
        return context
            .temp_tips
            .get(&parent.branch)
            .cloned()
            .ok_or_else(|| {
                Error::InvalidPlan(format!("parent `{}` has no rewritten tip", parent.branch))
            });
    }

    let old_base = node.old_base().expect("dependent node has old base");
    if Some(old_base) == parent.old_base() {
        return context
            .selected_bases
            .get(&parent.branch)
            .cloned()
            .ok_or_else(|| {
                Error::InvalidPlan(format!("parent `{}` has no selected base", parent.branch))
            });
    }

    context.mappings.get(old_base).cloned().ok_or_else(|| {
        Error::InvalidPlan(format!(
            "old_base `{}` for branch `{}` was not mapped",
            old_base, node.branch
        ))
    })
}

fn temp_ref(plan: &Plan, branch: &str) -> String {
    format!(
        "refs/cascade/tmp/{}/{}",
        plan.plan_id,
        encode_component(branch)
    )
}

fn ensure_target_branches_not_checked_out(git: &Git, branches: &[String]) -> Result<()> {
    let checked_out = git.checked_out_branches()?;
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

fn branch_replays_for_apply(
    git: &Git,
    plan: &Plan,
    ordered: &[String],
) -> Result<BTreeMap<String, BranchReplay>> {
    let nodes = plan
        .nodes
        .iter()
        .map(|node| (node.branch.as_str(), node))
        .collect::<HashMap<_, _>>();
    let mut replays = BTreeMap::new();

    for branch in ordered {
        let node = nodes
            .get(branch.as_str())
            .ok_or_else(|| Error::InvalidPlan(format!("unknown node `{branch}` in order")))?;
        let expected_tip = git.local_branch_tip(&node.branch)?;
        if !git.is_ancestor(&node.old_tip, &expected_tip)? {
            return Err(Error::InvalidPlan(format!(
                "branch `{}` rewrote planned commits after plan generation: planned tip `{}` is not reachable from `{expected_tip}`",
                node.branch, node.old_tip
            )));
        }
        let extra_commits = git.rev_list_reverse(&node.old_tip, &expected_tip)?;
        if let Some(merge) = git.rev_list_merges(&node.old_tip, &expected_tip)?.first() {
            return Err(Error::InvalidPlan(format!(
                "branch `{}` added merge commit `{merge}` after plan generation; merge replay is not supported yet",
                node.branch
            )));
        }
        replays.insert(
            node.branch.clone(),
            BranchReplay {
                expected_tip,
                extra_commits,
            },
        );
    }

    Ok(replays)
}

fn replay_commits(node: &Node, branch_replays: &BTreeMap<String, BranchReplay>) -> Vec<String> {
    let mut commits = node.commits().to_vec();
    if let Some(replay) = branch_replays.get(&node.branch) {
        commits.extend(replay.extra_commits.iter().cloned());
    }
    commits
}

fn replay_commits_from_extra(
    node: &Node,
    extra_commits: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let mut commits = node.commits().to_vec();
    if let Some(extra) = extra_commits.get(&node.branch) {
        commits.extend(extra.iter().cloned());
    }
    commits
}

fn selected_bases_from_mappings(
    plan: &Plan,
    mappings: &BTreeMap<String, String>,
) -> HashMap<String, String> {
    plan.nodes
        .iter()
        .filter_map(|node| {
            mappings
                .get(node.old_base()?)
                .map(|base| (node.branch.clone(), base.clone()))
        })
        .collect()
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

fn cleanup_stale_worktree(git: &Git, worktree: &std::path::Path) -> Result<()> {
    if !worktree.exists() {
        return Ok(());
    }

    let _ = git.worktree_remove_force(worktree);
    if worktree.exists() {
        fs::remove_dir_all(worktree).map_err(|source| Error::IoWithPath {
            path: worktree.to_owned(),
            source,
        })?;
    }

    Ok(())
}

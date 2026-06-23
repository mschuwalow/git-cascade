use super::{Dependency, Node, PLAN_VERSION, Plan, PlanCommit, branches_in_topological_order};
use crate::git::Git;
use crate::model::{BranchName, CommitId};
use crate::{Error, Result};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Current state of a planned branch ref, as observed by [`validate_branch_refs`].
#[derive(Debug, Clone)]
pub struct BranchRef {
    /// The branch's current tip commit.
    pub expected_tip: CommitId,
    /// Commits added to the branch after plan generation, oldest first.
    pub extra_commits: Vec<PlanCommit>,
}

pub fn validate_plan(git: &Git, plan: &Plan) -> Result<()> {
    validate_shape(plan)?;
    validate_git_objects(git, plan)?;
    validate_git_ranges(git, plan)?;
    validate_parent_reachability(git, plan)?;

    Ok(())
}

pub fn validate_plan_for_apply(git: &Git, plan: &Plan) -> Result<()> {
    validate_plan(git, plan)?;
    validate_branch_refs(git, plan)?;

    Ok(())
}

/// Merge commits are dropped during replay. That is sound when the merged-in
/// history is upstream work: either part of the old tip (whose rewrite is the
/// new tip, so the replayed branch catches up by being based on it) or
/// already contained in the new tip. Foreign merges would silently lose the
/// merged-in work and are rejected.
pub fn validate_merge_parents_for_apply(
    git: &Git,
    plan: &Plan,
    branch_refs: &BTreeMap<BranchName, BranchRef>,
    new_tip: &CommitId,
) -> Result<()> {
    for node in &plan.nodes {
        let extras = branch_refs
            .get(&node.branch)
            .map(|branch_ref| branch_ref.extra_commits.as_slice())
            .unwrap_or_default();
        for commit in node.commits().iter().chain(extras) {
            for parent in commit.parents.iter().skip(1) {
                if git.is_ancestor(parent, &plan.source.tip)? || git.is_ancestor(parent, new_tip)? {
                    continue;
                }
                return invalid(format!(
                    "branch `{}` merges history at `{parent}` that is part of neither the old nor the new tip; rebase the branch to linearize it first",
                    node.branch
                ));
            }
        }
    }

    Ok(())
}

/// Checks that planned branches were not rewritten since plan generation and
/// returns each branch's current tip plus any commits added after planning.
pub fn validate_branch_refs(git: &Git, plan: &Plan) -> Result<BTreeMap<BranchName, BranchRef>> {
    let mut branch_refs = BTreeMap::new();

    for node in &plan.nodes {
        let tip = &node.tip;
        let actual = git.local_branch_tip(&node.branch)?;
        if !git.is_ancestor(tip, &actual)? {
            return invalid(format!(
                "branch `{}` rewrote planned commits after plan generation: planned tip `{}` is not reachable from `{actual}`",
                node.branch, tip
            ));
        }

        let extra_commits = first_parent_chain(git, tip, &actual)?;
        // Reachability alone is not enough: the planned tip must sit on the
        // current tip's first-parent chain, otherwise the "extra" commits are
        // foreign history that joined through a merge's second parent. The
        // interior of the chain is contiguous by construction of the
        // first-parent walk, and its last commit is `actual`, so checking the
        // oldest commit suffices.
        if let Some(first) = extra_commits.first()
            && first.parents.first() != Some(tip)
        {
            return invalid(format!(
                "branch `{}` no longer extends planned tip `{}` by first parent: commit `{}` added after plan generation starts elsewhere",
                node.branch, tip, first.oid
            ));
        }
        branch_refs.insert(
            node.branch.clone(),
            BranchRef {
                expected_tip: actual,
                extra_commits,
            },
        );
    }

    Ok(branch_refs)
}

fn validate_shape(plan: &Plan) -> Result<()> {
    if plan.version != PLAN_VERSION {
        return invalid(format!("unsupported plan version `{}`", plan.version));
    }
    let node_by_branch = node_by_branch(plan)?;
    if plan.source.name.is_empty() {
        return invalid("source name must not be empty");
    }
    if plan.source.base == plan.source.tip {
        return invalid("source base must differ from tip");
    }

    let dependency_set = dependency_set(plan)?;
    for node in &plan.nodes {
        if node.branch.is_empty() {
            return invalid("node branch names must not be empty");
        }
        if node.commits().is_empty() && node.base != node.tip {
            return invalid(format!(
                "empty node `{}` must have matching base and tip",
                node.branch
            ));
        }
        if !node.commits().is_empty()
            && node.commits().last().map(|commit| commit.oid.as_str()) != Some(node.tip.as_str())
        {
            return invalid(format!(
                "node `{}` tip must match the last commit",
                node.branch
            ));
        }
        for commit in node.commits() {
            if commit.parents.is_empty() {
                return invalid(format!(
                    "commit `{}` for branch `{}` has no recorded parents",
                    commit.oid, node.branch
                ));
            }
        }

        let Some(parent) = node.parent() else {
            continue;
        };
        if !node_by_branch.contains_key(parent) {
            return invalid(format!(
                "dependent node `{}` references unknown parent `{parent}`",
                node.branch
            ));
        }
        if !dependency_set.contains(&(parent, node.branch.as_str())) {
            return invalid(format!(
                "dependent node `{}` is missing dependency edge from `{parent}`",
                node.branch
            ));
        }
    }

    for dependency in &plan.dependencies {
        validate_dependency(dependency, &node_by_branch)?;
        let child = node_by_branch[dependency.child.as_str()];
        if child.parent() != Some(dependency.parent.as_str()) {
            return invalid(format!(
                "dependency edge `{} -> {}` does not match child parent field",
                dependency.parent, dependency.child
            ));
        }
    }

    branches_in_topological_order(plan)?;
    validate_default_fork_point_mappability(plan, &node_by_branch)?;

    Ok(())
}

fn validate_git_objects(git: &Git, plan: &Plan) -> Result<()> {
    let mut commits = HashSet::<&CommitId>::new();
    commits.insert(&plan.source.base);
    commits.insert(&plan.source.tip);

    for node in &plan.nodes {
        commits.insert(&node.tip);
        commits.insert(&node.base);
        for commit in node.commits() {
            commits.insert(&commit.oid);
        }
    }

    for commit in commits {
        if !git.commit_exists(commit)? {
            return invalid(format!("commit `{commit}` does not exist"));
        }
    }

    Ok(())
}

fn validate_git_ranges(git: &Git, plan: &Plan) -> Result<()> {
    for node in &plan.nodes {
        let tip = &node.tip;
        let base = &node.base;
        let actual = first_parent_chain(git, base, tip)?;
        if actual != node.commits() {
            return invalid(format!(
                "commit list for branch `{}` does not match {}..{}",
                node.branch, base, tip
            ));
        }
    }

    Ok(())
}

/// The first-parent chain of `base..tip`. Commits off the chain are reached
/// through merge second parents and are never replayed.
fn first_parent_chain(git: &Git, base: &CommitId, tip: &CommitId) -> Result<Vec<PlanCommit>> {
    Ok(git
        .rev_list_first_parent_with_parents(base, tip)?
        .into_iter()
        .map(|(oid, parents)| PlanCommit::new(oid, parents))
        .collect())
}

fn validate_parent_reachability(git: &Git, plan: &Plan) -> Result<()> {
    if !git.is_ancestor(&plan.source.base, &plan.source.tip)? {
        return invalid(format!(
            "source base `{}` is not reachable from tip `{}`",
            plan.source.base, plan.source.tip
        ));
    }

    let node_by_branch = node_by_branch(plan)?;
    for node in &plan.nodes {
        let base = &node.base;
        if node.is_root() {
            if base == &plan.source.base
                || !git.is_ancestor(&plan.source.base, base)?
                || !git.is_ancestor(base, &plan.source.tip)?
            {
                return invalid(format!(
                    "base `{}` for branch `{}` is outside source range {}..{}",
                    base, node.branch, plan.source.base, plan.source.tip
                ));
            }
            continue;
        }

        let parent = node_by_branch[node.parent().expect("dependent node has a parent")];
        let parent_tip = &parent.tip;
        if !git.is_ancestor(base, parent_tip)? {
            return invalid(format!(
                "base `{}` for branch `{}` is not reachable from parent `{}` tip `{}`",
                base, node.branch, parent.branch, parent_tip
            ));
        }
    }

    Ok(())
}

fn validate_default_fork_point_mappability(
    plan: &Plan,
    node_by_branch: &HashMap<&str, &Node>,
) -> Result<()> {
    for node in &plan.nodes {
        if node.is_root() {
            continue;
        }
        let parent_branch = node.parent().expect("dependent node has a parent");
        let parent = node_by_branch[parent_branch];
        let base = &node.base;
        let parent_base = parent.base();
        if base.as_str() != parent_base && !parent.contains_commit(base) {
            return invalid(format!(
                "base `{}` for branch `{}` cannot be mapped through parent `{}`",
                base, node.branch, parent.branch
            ));
        }
    }

    Ok(())
}

fn validate_dependency<'a>(
    dependency: &'a Dependency,
    node_by_branch: &HashMap<&'a str, &'a Node>,
) -> Result<()> {
    if dependency.parent == dependency.child {
        return invalid(format!(
            "dependency for branch `{}` cannot point to itself",
            dependency.child
        ));
    }
    if !node_by_branch.contains_key(dependency.parent.as_str()) {
        return invalid(format!(
            "dependency references unknown parent `{}`",
            dependency.parent
        ));
    }
    if !node_by_branch.contains_key(dependency.child.as_str()) {
        return invalid(format!(
            "dependency references unknown child `{}`",
            dependency.child
        ));
    }

    Ok(())
}

fn node_by_branch(plan: &Plan) -> Result<HashMap<&str, &Node>> {
    let mut nodes = HashMap::new();
    for node in &plan.nodes {
        if nodes.insert(node.branch.as_str(), node).is_some() {
            return invalid(format!("duplicate node for branch `{}`", node.branch));
        }
    }

    Ok(nodes)
}

fn dependency_set(plan: &Plan) -> Result<HashSet<(&str, &str)>> {
    let mut dependencies = HashSet::new();
    for dependency in &plan.dependencies {
        if !dependencies.insert((dependency.parent.as_str(), dependency.child.as_str())) {
            return invalid(format!(
                "duplicate dependency edge `{} -> {}`",
                dependency.parent, dependency.child
            ));
        }
    }

    Ok(dependencies)
}

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::InvalidPlan(message.into()))
}

use super::{Dependency, Node, Plan, branches_in_topological_order};
use crate::git::Git;
use crate::{Error, Result};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Current state of a planned branch ref, as observed by [`validate_branch_refs`].
#[derive(Debug, Clone)]
pub struct BranchRef {
    /// The branch's current tip commit.
    pub expected_tip: String,
    /// Commits added to the branch after plan generation, oldest first.
    pub extra_commits: Vec<String>,
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

/// Checks that planned branches were not rewritten since plan generation and
/// returns each branch's current tip plus any commits added after planning.
pub fn validate_branch_refs(git: &Git, plan: &Plan) -> Result<BTreeMap<String, BranchRef>> {
    let mut branch_refs = BTreeMap::new();
    for node in &plan.nodes {
        let tip = node.tip.as_str();
        let actual = git.local_branch_tip(&node.branch)?;
        if !git.is_ancestor(tip, &actual)? {
            return invalid(format!(
                "branch `{}` rewrote planned commits after plan generation: planned tip `{}` is not reachable from `{actual}`",
                node.branch, tip
            ));
        }

        let merges = git.rev_list_merges(tip, &actual)?;
        if let Some(merge) = merges.first() {
            return invalid(format!(
                "branch `{}` added merge commit `{merge}` after plan generation; merge replay is not supported yet",
                node.branch
            ));
        }

        let extra_commits = git.rev_list_reverse(tip, &actual)?;
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
    if plan.version != 1 {
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
        if node.commits().is_empty() {
            return invalid(format!(
                "node `{}` must contain at least one commit",
                node.branch
            ));
        }
        if node.commits().last() != Some(&node.tip) {
            return invalid(format!(
                "node `{}` tip must match the last commit",
                node.branch
            ));
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
    let mut commits = HashSet::<&str>::new();
    commits.insert(plan.source.base.as_str());
    commits.insert(plan.source.tip.as_str());

    for node in &plan.nodes {
        commits.insert(node.tip.as_str());
        commits.insert(node.base());
        for commit in node.commits() {
            commits.insert(commit.as_str());
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
        let tip = node.tip.as_str();
        let base = node.base();
        let commits = git.rev_list_reverse(base, tip)?;
        if commits != node.commits() {
            return invalid(format!(
                "commit list for branch `{}` does not match {}..{}",
                node.branch, base, tip
            ));
        }

        let merges = git.rev_list_merges(base, tip)?;
        if let Some(merge) = merges.first() {
            return invalid(format!(
                "branch `{}` contains merge commit `{merge}`; merge replay is not supported yet",
                node.branch
            ));
        }
    }

    Ok(())
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
        let base = node.base();
        if node.is_root() {
            if base == plan.source.base
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
        let parent_tip = parent.tip.as_str();
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
        let base = node.base();
        let parent_base = parent.base();
        if base != parent_base && !parent.commits().iter().any(|commit| commit == base) {
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

use super::{Dependency, Node, PLAN_VERSION, Plan, PlanCommit, branches_in_topological_order};
use crate::git::Git;
use crate::{Error, Result};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Current state of a planned branch ref, as observed by [`validate_branch_refs`].
#[derive(Debug, Clone)]
pub struct BranchRef {
    /// The branch's current tip commit.
    pub expected_tip: String,
    /// Commits added to the branch after plan generation, oldest first.
    pub extra_commits: Vec<PlanCommit>,
}

pub fn validate_plan(git: &Git, plan: &Plan) -> Result<()> {
    validate_shape(plan)?;
    validate_git_objects(git, plan)?;
    validate_git_ranges(git, plan)?;
    validate_parent_reachability(git, plan)?;
    validate_commit_parent_mappability(plan)?;

    Ok(())
}

pub fn validate_plan_for_apply(git: &Git, plan: &Plan) -> Result<()> {
    validate_plan(git, plan)?;
    validate_branch_refs(git, plan)?;

    Ok(())
}

/// Structurally unmapped parents are kept identically, which is only sound
/// when the new tip still reaches them.
pub fn validate_unmapped_parents_for_apply(
    git: &Git,
    plan: &Plan,
    branch_refs: &BTreeMap<String, BranchRef>,
    new_tip: &str,
) -> Result<()> {
    let node_by_branch = node_by_branch(plan)?;

    for node in &plan.nodes {
        let mut mappable = chain_mappable_oids(plan, node, &node_by_branch);
        // The chain root's parent is substituted with the replay base.
        if let Some(chain_root) = super::first_parent_chain_root(node.commits())
            && let Some(fork_parent) = chain_root.first_parent()
        {
            mappable.insert(fork_parent.to_owned());
        }
        for ancestor_branch in ancestor_chain(node, &node_by_branch) {
            if let Some(ancestor_ref) = branch_refs.get(ancestor_branch) {
                for extra in &ancestor_ref.extra_commits {
                    mappable.insert(extra.oid.clone());
                }
            }
        }
        let extras = branch_refs
            .get(&node.branch)
            .map(|branch_ref| branch_ref.extra_commits.as_slice())
            .unwrap_or_default();
        for commit in node.commits().iter().chain(extras) {
            for parent in &commit.parents {
                if mappable.contains(parent) {
                    continue;
                }
                if git.is_ancestor(parent, &plan.source.tip)?
                    && !git.is_ancestor(parent, &plan.source.base)?
                    && !git.is_ancestor(parent, new_tip)?
                {
                    return invalid(format!(
                        "commit `{}` for branch `{}` has parent `{parent}` inside the rewritten range {}..{} that is not retained by the new tip; its rewritten counterpart is unknown",
                        commit.oid, node.branch, plan.source.base, plan.source.tip
                    ));
                }
            }
            mappable.insert(commit.oid.clone());
        }
    }

    Ok(())
}

/// Checks that planned branches were not rewritten since plan generation and
/// returns each branch's current tip plus any commits added after planning.
pub fn validate_branch_refs(git: &Git, plan: &Plan) -> Result<BTreeMap<String, BranchRef>> {
    let node_by_branch = node_by_branch(plan)?;
    let all_node_commits = all_node_commit_oids(plan);
    let ordered = branches_in_topological_order(plan)?;
    let mut branch_refs: BTreeMap<String, BranchRef> = BTreeMap::new();

    for branch in &ordered {
        let node = node_by_branch[branch.as_str()];
        let tip = node.tip.as_str();
        let actual = git.local_branch_tip(&node.branch)?;
        if !git.is_ancestor(tip, &actual)? {
            return invalid(format!(
                "branch `{}` rewrote planned commits after plan generation: planned tip `{}` is not reachable from `{actual}`",
                node.branch, tip
            ));
        }

        let extra_commits = git
            .rev_list_with_parents(tip, &actual)?
            .into_iter()
            .map(|(oid, parents)| PlanCommit::new(oid, parents))
            .collect::<Vec<_>>();

        // Extra commits must satisfy the same parent-mappability conditions
        // as planned commits, evaluated against planned commits, earlier
        // extras, and the ancestor chain (including its extras).
        let mut mappable = chain_mappable_oids(plan, node, &node_by_branch);
        for oid in node.commit_oids() {
            mappable.insert(oid.to_owned());
        }
        for ancestor_branch in ancestor_chain(node, &node_by_branch) {
            if let Some(ancestor_ref) = branch_refs.get(ancestor_branch) {
                for extra in &ancestor_ref.extra_commits {
                    mappable.insert(extra.oid.clone());
                }
            }
        }
        for commit in &extra_commits {
            validate_parents_mappable(&node.branch, commit, &mappable, &all_node_commits)?;
            mappable.insert(commit.oid.clone());
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
    if plan.version < 1 || plan.version > PLAN_VERSION {
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
        if node.commits().last().map(|commit| commit.oid.as_str()) != Some(node.tip.as_str()) {
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
            if commit.parents.len() > 2 {
                return invalid(format!(
                    "commit `{}` for branch `{}` is an octopus merge; octopus merges are not supported",
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
    let mut commits = HashSet::<&str>::new();
    commits.insert(plan.source.base.as_str());
    commits.insert(plan.source.tip.as_str());

    for node in &plan.nodes {
        commits.insert(node.tip.as_str());
        commits.insert(node.base());
        for commit in node.commits() {
            commits.insert(commit.oid.as_str());
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
        let actual = git
            .rev_list_with_parents(base, tip)?
            .into_iter()
            .map(|(oid, parents)| PlanCommit::new(oid, parents))
            .collect::<Vec<_>>();
        if actual != node.commits() {
            return invalid(format!(
                "commit list for branch `{}` does not match {}..{}",
                node.branch, base, tip
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

/// Merge parents may only reference commits with a known mapping (own chain,
/// ancestor nodes, the source tip) or history outside every node.
fn validate_commit_parent_mappability(plan: &Plan) -> Result<()> {
    let node_by_branch = node_by_branch(plan)?;
    let all_node_commits = all_node_commit_oids(plan);

    for node in &plan.nodes {
        let mut mappable = chain_mappable_oids(plan, node, &node_by_branch);
        for commit in node.commits() {
            validate_parents_mappable(&node.branch, commit, &mappable, &all_node_commits)?;
            mappable.insert(commit.oid.clone());
        }
    }

    Ok(())
}

fn validate_parents_mappable(
    branch: &str,
    commit: &PlanCommit,
    mappable: &HashSet<String>,
    all_node_commits: &HashSet<&str>,
) -> Result<()> {
    if commit.parents.len() > 2 {
        return invalid(format!(
            "commit `{}` for branch `{branch}` is an octopus merge; octopus merges are not supported",
            commit.oid
        ));
    }

    for parent in &commit.parents {
        if mappable.contains(parent) {
            continue;
        }
        if all_node_commits.contains(parent.as_str()) {
            return invalid(format!(
                "commit `{}` for branch `{branch}` has parent `{parent}` in another branch's planned commits; only ancestor-chain branches can be merge parents",
                commit.oid
            ));
        }
    }

    Ok(())
}

/// Oids that are guaranteed to have a mapping before `node` replays: its own
/// base plus the planned commits and bases of its ancestor chain.
fn chain_mappable_oids(
    plan: &Plan,
    node: &Node,
    node_by_branch: &HashMap<&str, &Node>,
) -> HashSet<String> {
    let mut mappable = HashSet::new();
    mappable.insert(node.base().to_owned());
    mappable.insert(plan.source.tip.clone());
    for ancestor_branch in ancestor_chain(node, node_by_branch) {
        let ancestor = node_by_branch[ancestor_branch];
        mappable.insert(ancestor.base().to_owned());
        for oid in ancestor.commit_oids() {
            mappable.insert(oid.to_owned());
        }
    }

    mappable
}

/// Branch names of `node`'s ancestors, nearest first.
fn ancestor_chain<'a>(node: &'a Node, node_by_branch: &HashMap<&str, &'a Node>) -> Vec<&'a str> {
    let mut chain = Vec::new();
    let mut current = node.parent();
    while let Some(branch) = current {
        let Some(ancestor) = node_by_branch.get(branch) else {
            break;
        };
        chain.push(ancestor.branch.as_str());
        current = ancestor.parent();
    }

    chain
}

fn all_node_commit_oids(plan: &Plan) -> HashSet<&str> {
    plan.nodes
        .iter()
        .flat_map(|node| node.commit_oids())
        .collect()
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
        if base != parent_base && !parent.contains_commit(base) {
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

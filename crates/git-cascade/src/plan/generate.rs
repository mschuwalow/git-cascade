use super::validate::validate_plan;
use super::{Dependency, Node, Plan, PlanId, PlanName, Repository, Source};
use crate::git::{Git, LocalBranch};
use crate::storage::Storage;
use crate::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct GenerateOptions {
    pub name: PlanName,
    pub old_base: String,
    pub old_tip: String,
    pub excluded_branches: Vec<String>,
}

#[derive(Debug, Clone)]
struct Candidate {
    branch: LocalBranch,
    parent_branch: Option<String>,
    old_base: String,
    commits: Vec<String>,
}

/// Memoizes the pairwise git queries used during candidate discovery, which
/// would otherwise be repeated for every candidate selection round.
struct GitQueries<'a> {
    git: &'a Git,
    merge_bases: HashMap<(String, String), Option<String>>,
    ancestors: HashMap<(String, String), bool>,
    owned_commits: HashMap<(String, String), Vec<String>>,
}

impl<'a> GitQueries<'a> {
    fn new(git: &'a Git) -> Self {
        Self {
            git,
            merge_bases: HashMap::new(),
            ancestors: HashMap::new(),
            owned_commits: HashMap::new(),
        }
    }

    fn merge_base(&mut self, left: &str, right: &str) -> Result<Option<String>> {
        let key = (left.to_owned(), right.to_owned());
        if let Some(result) = self.merge_bases.get(&key) {
            return Ok(result.clone());
        }

        let result = self.git.merge_base(left, right)?;
        self.merge_bases.insert(key, result.clone());
        Ok(result)
    }

    fn is_ancestor(&mut self, ancestor: &str, descendant: &str) -> Result<bool> {
        let key = (ancestor.to_owned(), descendant.to_owned());
        if let Some(result) = self.ancestors.get(&key) {
            return Ok(*result);
        }

        let result = self.git.is_ancestor(ancestor, descendant)?;
        self.ancestors.insert(key, result);
        Ok(result)
    }

    fn owned_commits(&mut self, base: &str, tip: &str, branch: &str) -> Result<Vec<String>> {
        let key = (base.to_owned(), tip.to_owned());
        if let Some(result) = self.owned_commits.get(&key) {
            return Ok(result.clone());
        }

        let merges = self.git.rev_list_merges(base, tip)?;
        if let Some(merge) = merges.first() {
            return Err(Error::Unsupported(format!(
                "branch `{branch}` contains merge commit `{merge}`; merge replay is not supported yet"
            )));
        }

        let result = self.git.rev_list_reverse(base, tip)?;
        self.owned_commits.insert(key, result.clone());
        Ok(result)
    }
}

pub fn generate_stored_plan(
    git: &Git,
    storage: &Storage,
    options: &GenerateOptions,
    replace: bool,
) -> Result<Plan> {
    let plan = generate_plan(git, options)?;
    validate_plan(git, &plan)?;
    write_named_plan(storage, &options.name, &plan, replace)?;
    Ok(plan)
}

pub fn generate_plan(git: &Git, options: &GenerateOptions) -> Result<Plan> {
    let name = &options.name;
    let old_tip_ref = options.old_tip.as_str();
    let old_tip = git.resolve_commit(old_tip_ref)?;
    let old_base_tip = git.resolve_commit(&options.old_base)?;
    let old_base = old_range_base(
        git,
        name.as_str(),
        &old_tip,
        &options.old_base,
        &old_base_tip,
    )?;

    let mut nodes = Vec::new();
    let mut dependencies = Vec::new();
    let mut assigned = HashSet::new();
    if let Some(local_branch) = old_tip_local_branch(git, old_tip_ref)? {
        assigned.insert(local_branch);
    }
    assigned.extend(options.excluded_branches.iter().cloned());
    let branches = git.local_branches()?;
    let mut queries = GitQueries::new(git);

    while let Some(candidate) = next_candidate(
        &mut queries,
        &branches,
        &nodes,
        &assigned,
        &old_base,
        &old_tip,
    )? {
        assigned.insert(candidate.branch.name.clone());
        if let Some(parent_branch) = &candidate.parent_branch {
            dependencies.push(Dependency {
                parent: parent_branch.clone(),
                child: candidate.branch.name.clone(),
            });
        }
        nodes.push(Node {
            branch: candidate.branch.name,
            tip: candidate.branch.tip,
            base: candidate.old_base,
            commits: candidate.commits,
            parent: candidate.parent_branch,
        });
    }

    let plan_id = PlanId::new();

    Ok(Plan {
        version: 1,
        plan_id,
        generated_at: OffsetDateTime::now_utc(),
        repository: Repository {
            git_dir: git.git_common_dir()?.display().to_string(),
            head_at_generation: git.head_oid()?,
        },
        source: Source {
            name: name.to_string(),
            base: old_base,
            tip: old_tip,
        },
        nodes,
        dependencies,
    })
}

fn old_range_base(
    git: &Git,
    name: &str,
    old_tip: &str,
    old_base_input: &str,
    old_base_tip: &str,
) -> Result<String> {
    git.merge_base(old_tip, old_base_tip)?.ok_or_else(|| {
        Error::InvalidInvocation(format!(
            "old base `{old_base_input}` has no merge base with old tip for plan `{name}`"
        ))
    })
}

fn old_tip_local_branch(git: &Git, old_tip: &str) -> Result<Option<String>> {
    Ok(git
        .symbolic_full_name(old_tip)?
        .and_then(|refname| refname.strip_prefix("refs/heads/").map(str::to_owned)))
}

fn write_named_plan(storage: &Storage, name: &PlanName, plan: &Plan, replace: bool) -> Result<()> {
    if storage.state_path().exists() {
        return Err(Error::ActiveOperation {
            path: storage.state_path(),
        });
    }

    storage.ensure_plans_dir()?;
    let path = storage.plan_path(name);
    if path.exists() && !replace {
        return Err(Error::PlanExists {
            name: name.to_string(),
            path,
        });
    }

    let yaml = serde_yaml::to_string(plan)?;
    let mut options = fs::OpenOptions::new();
    options.write(true);
    if replace {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }

    let mut file = options
        .open(&path)
        .map_err(|source| Error::IoWithPath { path, source })?;
    file.write_all(yaml.as_bytes())?;
    Ok(())
}

fn next_candidate(
    queries: &mut GitQueries<'_>,
    branches: &[LocalBranch],
    nodes: &[Node],
    assigned: &HashSet<String>,
    source_old_base: &str,
    source_old_tip: &str,
) -> Result<Option<Candidate>> {
    let node_by_branch = nodes
        .iter()
        .map(|node| (node.branch.as_str(), node))
        .collect::<HashMap<_, _>>();
    let mut best: Option<Candidate> = None;

    for branch in branches {
        if assigned.contains(&branch.name) {
            continue;
        }

        if let Some(base) = queries.merge_base(source_old_tip, &branch.tip)?
            && !queries.is_ancestor(&branch.tip, source_old_tip)?
            && base != source_old_base
            && queries.is_ancestor(source_old_base, &base)?
        {
            let commits = queries.owned_commits(&base, &branch.tip, &branch.name)?;
            if !commits.is_empty() {
                let candidate = Candidate {
                    branch: branch.clone(),
                    parent_branch: None,
                    old_base: base,
                    commits,
                };
                if is_better_candidate(&candidate, best.as_ref(), &node_by_branch) {
                    best = Some(candidate);
                }
            }
        }

        for parent in nodes {
            let Some(base) = queries.merge_base(&parent.tip, &branch.tip)? else {
                continue;
            };

            if !parent.commits().contains(&base) {
                continue;
            }

            let commits = queries.owned_commits(&base, &branch.tip, &branch.name)?;
            if commits.is_empty() {
                continue;
            }

            let candidate = Candidate {
                branch: branch.clone(),
                parent_branch: Some(parent.branch.clone()),
                old_base: base,
                commits,
            };

            if is_better_candidate(&candidate, best.as_ref(), &node_by_branch) {
                best = Some(candidate);
            }
        }
    }

    Ok(best)
}

fn is_better_candidate(
    candidate: &Candidate,
    current: Option<&Candidate>,
    node_by_branch: &HashMap<&str, &Node>,
) -> bool {
    let Some(current) = current else {
        return true;
    };

    let candidate_parent_depth = candidate
        .parent_branch
        .as_deref()
        .map_or(0, |parent| parent_depth(parent, node_by_branch) + 1);
    let current_parent_depth = current
        .parent_branch
        .as_deref()
        .map_or(0, |parent| parent_depth(parent, node_by_branch) + 1);
    candidate_parent_depth
        .cmp(&current_parent_depth)
        .then_with(|| current.commits.len().cmp(&candidate.commits.len()))
        .then_with(|| current.branch.name.cmp(&candidate.branch.name))
        .is_gt()
}

fn parent_depth(branch: &str, node_by_branch: &HashMap<&str, &Node>) -> usize {
    let mut depth = 0;
    let mut current = node_by_branch.get(branch).copied();
    while let Some(node) = current {
        let Some(parent) = node.parent() else {
            break;
        };
        depth += 1;
        current = node_by_branch.get(parent).copied();
    }
    depth
}

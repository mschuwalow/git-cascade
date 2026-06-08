use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::git::{Git, LocalBranch};
use crate::plan::{Dependency, Node, NodeKind, Plan, Repository, Source};
use crate::plan_validate::validate_plan;
use crate::storage::{PlanKey, Storage};
use crate::{Error, Result};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct GenerateOptions {
    pub anchor_branch: String,
    pub replace: bool,
}

#[derive(Debug, Clone)]
struct Candidate {
    branch: LocalBranch,
    parent_branch: String,
    old_base: String,
    commits: Vec<String>,
}

pub fn generate_anchor_keyed_plan(
    git: &Git,
    storage: &Storage,
    options: GenerateOptions,
) -> Result<Plan> {
    let plan = generate_plan(git, &options.anchor_branch)?;
    validate_plan(git, &plan)?;
    let key = PlanKey::from_anchor(&options.anchor_branch)?;
    write_anchor_keyed_plan(storage, &key, &plan, options.replace)?;
    Ok(plan)
}

pub fn generate_plan(git: &Git, anchor_branch: &str) -> Result<Plan> {
    let anchor_tip = git.local_branch_tip(anchor_branch)?;

    let mut nodes = vec![Node {
        branch: anchor_branch.to_owned(),
        old_tip: anchor_tip.clone(),
        kind: NodeKind::Anchor,
    }];
    let mut dependencies = Vec::new();
    let mut assigned = HashSet::from([anchor_branch.to_owned()]);
    let branches = git.local_branches()?;

    while let Some(candidate) = next_candidate(git, &branches, &nodes, &assigned)? {
        assigned.insert(candidate.branch.name.clone());
        dependencies.push(Dependency {
            parent: candidate.parent_branch.clone(),
            child: candidate.branch.name.clone(),
        });
        nodes.push(Node {
            branch: candidate.branch.name,
            old_tip: candidate.branch.tip,
            kind: NodeKind::Dependent {
                parent: candidate.parent_branch,
                old_base: candidate.old_base,
                commits: candidate.commits,
            },
        });
    }

    let now = OffsetDateTime::now_utc();
    let generated_at = now.format(&Rfc3339).map_err(|error| {
        Error::Unsupported(format!("failed to format generation timestamp: {error}"))
    })?;
    let plan_id = Uuid::new_v4().to_string();

    Ok(Plan {
        version: 1,
        plan_id,
        generated_at,
        repository: Repository {
            git_dir: git.git_common_dir()?.display().to_string(),
            head_at_generation: git.head_oid()?,
        },
        source: Source {
            anchor_branch: anchor_branch.to_owned(),
            anchor_old_tip: anchor_tip,
        },
        nodes,
        dependencies,
    })
}

fn write_anchor_keyed_plan(
    storage: &Storage,
    key: &PlanKey,
    plan: &Plan,
    replace: bool,
) -> Result<()> {
    if storage.state_path().exists() {
        return Err(Error::ActiveOperation {
            path: storage.state_path(),
        });
    }

    storage.ensure_plans_dir()?;
    let path = storage.plan_path(key);
    if path.exists() && !replace {
        return Err(Error::PlanExists {
            key: key.to_string(),
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
    git: &Git,
    branches: &[LocalBranch],
    nodes: &[Node],
    assigned: &HashSet<String>,
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

        for parent in nodes {
            let Some(base) = git.merge_base(&parent.old_tip, &branch.tip)? else {
                continue;
            };

            if parent.is_anchor() {
                if git.is_ancestor(&branch.tip, &parent.old_tip)? {
                    continue;
                }
            } else {
                let parent_old_base = parent.old_base().expect("dependent parent has an old base");
                if base != parent_old_base && !parent.commits().contains(&base) {
                    continue;
                }
            }

            let commits = owned_commits(git, &base, &branch.tip, &branch.name)?;
            if commits.is_empty() {
                continue;
            }

            let candidate = Candidate {
                branch: branch.clone(),
                parent_branch: parent.branch.clone(),
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

    let candidate_parent_depth = parent_depth(&candidate.parent_branch, node_by_branch);
    let current_parent_depth = parent_depth(&current.parent_branch, node_by_branch);
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

fn owned_commits(git: &Git, base: &str, tip: &str, branch: &str) -> Result<Vec<String>> {
    let merges = git.rev_list_merges(base, tip)?;
    if let Some(merge) = merges.first() {
        return Err(Error::Unsupported(format!(
            "branch `{branch}` contains merge commit `{merge}`; merge replay is not supported yet"
        )));
    }

    git.rev_list_reverse(base, tip)
}

use super::handle_replay_outcome;
use super::landed as landed_inference;
use crate::git::Git;
use crate::model::{BranchName, GitRef, Strategy};
use crate::plan::{GenerateOptions, PlanName, generate_plan, generate_stored_plan};
use crate::replay::{ReplayMode, ReplayOptions, dry_run, execute};
use crate::storage::Storage;
use crate::{Error, Result};

pub(super) struct RunOptions {
    pub(super) strategy: Strategy,
    pub(super) is_dry_run: bool,
    pub(super) in_place: bool,
    pub(super) replay_mode: ReplayMode,
}

pub(super) fn restack(
    branch: Option<BranchName>,
    base: Option<GitRef>,
    run: RunOptions,
) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let branch = branch.or(git.current_branch()?).ok_or_else(|| {
        Error::InvalidInvocation("restack needs a branch when HEAD is detached".to_owned())
    })?;
    let old_base = match base {
        Some(base) => base,
        None => infer_old_base_from_default_branch(&git, &branch)?,
    };
    let branch_ref = GitRef::from(branch.clone());
    let excluded_branches = excluded_target_branches(&git, &branch_ref)?;
    let plan_name = generated_plan_name("restack", branch.as_str())?;

    generate_and_apply(GeneratedApply {
        git: &git,
        storage: &storage,
        generate: GenerateOptions {
            name: plan_name,
            old_base,
            old_tip: branch_ref.clone(),
            excluded_branches,
        },
        new_tip: branch_ref,
        run,
        success_message: "restacked dependent branches",
    })
}

pub(super) fn replay(
    old_tip: GitRef,
    old_base: GitRef,
    new_tip: GitRef,
    run: RunOptions,
) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let excluded_branches = excluded_target_branches(&git, &new_tip)?;
    let plan_name = generated_plan_name("replay", old_tip.as_str())?;

    generate_and_apply(GeneratedApply {
        git: &git,
        storage: &storage,
        generate: GenerateOptions {
            name: plan_name,
            old_base,
            old_tip,
            excluded_branches,
        },
        new_tip,
        run,
        success_message: "replayed dependent branches",
    })
}

pub(super) fn sync(
    base: Option<GitRef>,
    oldest_branch: Option<GitRef>,
    run: RunOptions,
) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let base = base
        .or(git.default_branch_ref()?.map(GitRef::new))
        .ok_or_else(|| {
            Error::InvalidInvocation(
                "sync needs --base <ref> when no default branch exists".to_owned(),
            )
        })?;
    let old_base = match oldest_branch {
        Some(oldest_branch) => infer_old_base_from_branch(&git, &base, &oldest_branch)?,
        None => infer_old_base_from_local_fork_points(&git, &base)?,
    };
    let excluded_branches = excluded_target_branches(&git, &base)?;
    let plan_name = generated_plan_name("sync", base.as_str())?;

    generate_and_apply(GeneratedApply {
        git: &git,
        storage: &storage,
        generate: GenerateOptions {
            name: plan_name,
            old_base,
            old_tip: base.clone(),
            excluded_branches,
        },
        new_tip: base,
        run,
        success_message: "synced dependent branches",
    })
}

pub(super) fn landed(
    old_tip: GitRef,
    onto: Option<GitRef>,
    old_base: Option<GitRef>,
    run: RunOptions,
) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let onto = onto
        .or(git.default_branch_ref()?.map(GitRef::new))
        .ok_or_else(|| {
            Error::InvalidInvocation(
                "landed needs --onto <ref> when no default branch exists".to_owned(),
            )
        })?;
    let inference = landed_inference::infer_range(&git, &old_tip, &onto, old_base)?;
    let excluded_branches = excluded_target_branches(&git, &onto)?;
    let plan_name = generated_plan_name("landed", old_tip.as_str())?;

    generate_and_apply(GeneratedApply {
        git: &git,
        storage: &storage,
        generate: GenerateOptions {
            name: plan_name,
            old_base: inference.old_base,
            old_tip,
            excluded_branches,
        },
        new_tip: inference.new_tip,
        run,
        success_message: "updated dependents of landed branch",
    })
}

fn infer_old_base_from_branch(git: &Git, onto: &GitRef, oldest_branch: &GitRef) -> Result<GitRef> {
    let onto_tip = git.resolve_commit(onto)?;
    let branch_tip = git.resolve_commit(oldest_branch)?;
    let fork_point = git
        .unique_merge_base(&onto_tip, &branch_tip)?
        .ok_or_else(|| {
            Error::InvalidInvocation(format!(
                "cannot infer old base for sync; `{oldest_branch}` has no merge base with `{onto}`"
            ))
        })?;

    git.commit_parents(&fork_point)?.first().cloned().map(GitRef::from).ok_or_else(|| {
        Error::InvalidInvocation(format!(
            "cannot infer old base for sync; oldest fork point `{fork_point}` has no parent. Use `git cascade replay --old-base <ref> --old-tip {onto} --new-tip {onto}`."
        ))
    })
}

fn infer_old_base_from_local_fork_points(git: &Git, onto: &GitRef) -> Result<GitRef> {
    let onto_tip = git.resolve_commit(onto)?;
    let excluded_branches = excluded_target_branches(git, onto)?;
    let mut oldest_fork_point = None;

    for branch in git.local_branches()? {
        if excluded_branches
            .iter()
            .any(|excluded| excluded == &branch.name)
            || git.is_ancestor(&branch.tip, &onto_tip)?
        {
            continue;
        }

        // Criss-cross branches are skipped here and warned about during
        // plan generation.
        let mut bases = git.merge_bases_all(&onto_tip, &branch.tip)?;
        if bases.len() != 1 {
            continue;
        }
        let fork_point = bases.remove(0);
        if fork_point == onto_tip {
            continue;
        }

        oldest_fork_point = Some(if let Some(current) = oldest_fork_point {
            git.unique_merge_base(&current, &fork_point)?
                .unwrap_or(current)
        } else {
            fork_point
        });
    }

    let base = oldest_fork_point.unwrap_or_else(|| onto_tip.clone());
    git.commit_parents(&base)?.first().cloned().map(GitRef::from).ok_or_else(|| {
        Error::InvalidInvocation(format!(
            "cannot infer old base for sync; oldest fork point `{base}` has no parent. Use `git cascade replay --old-base <ref> --old-tip {onto} --new-tip {onto}`."
        ))
    })
}

struct GeneratedApply<'a> {
    git: &'a Git,
    storage: &'a Storage,
    generate: GenerateOptions,
    new_tip: GitRef,
    run: RunOptions,
    success_message: &'static str,
}

fn generate_and_apply(options: GeneratedApply<'_>) -> Result<()> {
    if options.run.is_dry_run {
        let plan = generate_plan(options.git, &options.generate)?;
        print!(
            "{}",
            dry_run(
                options.git,
                options.storage,
                &plan,
                ReplayOptions {
                    plan_name: options.generate.name,
                    new_tip_input: options.new_tip,
                    strategy: options.run.strategy,
                    in_place: options.run.in_place,
                    replay_mode: options.run.replay_mode,
                },
            )?
        );
        return Ok(());
    }

    let plan = generate_stored_plan(options.git, options.storage, &options.generate, false)?;

    let outcome = execute(
        options.git,
        options.storage,
        &plan,
        ReplayOptions {
            plan_name: options.generate.name.clone(),
            new_tip_input: options.new_tip,
            strategy: options.run.strategy,
            in_place: options.run.in_place,
            replay_mode: options.run.replay_mode,
        },
    )?;

    handle_replay_outcome(outcome, options.success_message)
}

fn generated_plan_name(kind: &str, label: &str) -> Result<PlanName> {
    PlanName::new(format!("generated/{kind}/{label}/{}", uuid::Uuid::new_v4()))
}

fn infer_old_base_from_default_branch(git: &Git, old_tip: &BranchName) -> Result<GitRef> {
    if let Some(default_tip) = git.origin_default_branch_tip()? {
        return Ok(default_tip.into());
    }

    if let Some(default_tip) = git.local_default_branch_tip()? {
        return Ok(default_tip.into());
    }

    Err(Error::InvalidInvocation(format!(
        "cannot infer base branch for restack from old tip `{old_tip}`; pass --base <ref>"
    )))
}

fn excluded_target_branches(git: &Git, target: &GitRef) -> Result<Vec<BranchName>> {
    let mut branches = Vec::new();
    if let Some(refname) = git.symbolic_full_name(target)? {
        if let Some(branch) = refname.strip_prefix("refs/heads/") {
            branches.push(BranchName::from_git_unchecked(branch));
        } else if let Some(remote_ref) = refname.strip_prefix("refs/remotes/")
            && let Some((_, branch)) = remote_ref.split_once('/')
        {
            branches.push(BranchName::from_git_unchecked(branch));
        }
    } else if let Some(branch) = target.as_str().strip_prefix("origin/") {
        branches.push(BranchName::from_git_unchecked(branch));
    }

    branches.sort();
    branches.dedup();
    Ok(branches)
}

use super::landed as landed_inference;
use crate::apply::{ApplyOptions, dry_run, execute};
use crate::git::Git;
use crate::plan::{GenerateOptions, PlanName, generate_plan, generate_stored_plan};
use crate::state::Strategy;
use crate::storage::Storage;
use crate::{Error, Result};

pub(super) struct RunOptions {
    pub(super) strategy: Strategy,
    pub(super) is_dry_run: bool,
    pub(super) in_place: bool,
}

impl RunOptions {
    pub(super) fn move_to_current_tips(is_dry_run: bool, in_place: bool) -> Self {
        Self {
            strategy: Strategy::MoveToCurrentTips,
            is_dry_run,
            in_place,
        }
    }
}

pub(super) fn restack(branch: Option<String>, base: Option<String>, run: RunOptions) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let branch = branch.or(git.current_branch()?).ok_or_else(|| {
        Error::InvalidInvocation("restack needs a branch when HEAD is detached".to_owned())
    })?;
    let old_base = match base {
        Some(base) => base,
        None => infer_old_base_from_default_branch(&git, &branch)?,
    };
    let excluded_branches = excluded_target_branches(&git, &branch)?;
    let plan_name = generated_plan_name("restack", &branch)?;

    generate_and_apply(GeneratedApply {
        git: &git,
        storage: &storage,
        generate: GenerateOptions {
            name: plan_name,
            old_base,
            old_tip: branch.clone(),
            excluded_branches,
        },
        new_tip: branch,
        run,
        success_message: "restacked dependent branches",
    })
}

pub(super) fn replay(old_tip: &str, old_base: &str, new_tip: &str, run: RunOptions) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let excluded_branches = excluded_target_branches(&git, new_tip)?;
    let plan_name = generated_plan_name("replay", old_tip)?;

    generate_and_apply(GeneratedApply {
        git: &git,
        storage: &storage,
        generate: GenerateOptions {
            name: plan_name,
            old_base: old_base.to_owned(),
            old_tip: old_tip.to_owned(),
            excluded_branches,
        },
        new_tip: new_tip.to_owned(),
        run,
        success_message: "replayed dependent branches",
    })
}

pub(super) fn sync(base: Option<String>, run: RunOptions) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let base = base.or(git.default_branch_ref()?).ok_or_else(|| {
        Error::InvalidInvocation("sync needs --base <ref> when no default branch exists".to_owned())
    })?;
    let old_base = infer_old_base_from_local_fork_points(&git, &base)?;
    let excluded_branches = excluded_target_branches(&git, &base)?;
    let plan_name = generated_plan_name("sync", &base)?;

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
    old_tip: &str,
    onto: Option<String>,
    old_base: Option<String>,
    run: RunOptions,
) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let onto = onto.or(git.default_branch_ref()?).ok_or_else(|| {
        Error::InvalidInvocation(
            "landed needs --onto <ref> when no default branch exists".to_owned(),
        )
    })?;
    let inference = landed_inference::infer_range(&git, old_tip, &onto, old_base)?;
    let excluded_branches = excluded_target_branches(&git, &onto)?;
    let plan_name = generated_plan_name("landed", old_tip)?;

    generate_and_apply(GeneratedApply {
        git: &git,
        storage: &storage,
        generate: GenerateOptions {
            name: plan_name,
            old_base: inference.old_base,
            old_tip: old_tip.to_owned(),
            excluded_branches,
        },
        new_tip: inference.new_tip,
        run,
        success_message: "updated dependents of landed branch",
    })
}

fn infer_old_base_from_local_fork_points(git: &Git, onto: &str) -> Result<String> {
    let onto_tip = git.resolve_commit(onto)?;
    let excluded_branches = excluded_target_branches(git, onto)?;
    let mut oldest_fork_point = None::<String>;

    for branch in git.local_branches()? {
        if excluded_branches.contains(&branch.name) || git.is_ancestor(&branch.tip, &onto_tip)? {
            continue;
        }

        let Some(fork_point) = git.merge_base(&onto_tip, &branch.tip)? else {
            continue;
        };
        if fork_point == onto_tip {
            continue;
        }

        oldest_fork_point = Some(if let Some(current) = oldest_fork_point {
            git.merge_base(&current, &fork_point)?.unwrap_or(current)
        } else {
            fork_point
        });
    }

    let base = oldest_fork_point.unwrap_or_else(|| onto_tip.clone());
    git.commit_parents(&base)?.first().cloned().ok_or_else(|| {
        Error::InvalidInvocation(format!(
            "cannot infer old base for sync; oldest fork point `{base}` has no parent. Use `git cascade replay --old-base <ref> --old-tip {onto} --new-tip {onto}`."
        ))
    })
}

struct GeneratedApply<'a> {
    git: &'a Git,
    storage: &'a Storage,
    generate: GenerateOptions,
    new_tip: String,
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
                ApplyOptions {
                    plan_name: options.generate.name,
                    new_tip_input: options.new_tip,
                    strategy: options.run.strategy,
                    in_place: options.run.in_place,
                },
            )?
        );
        return Ok(());
    }

    let plan = generate_stored_plan(options.git, options.storage, &options.generate, false)?;

    execute(
        options.git,
        options.storage,
        &plan,
        ApplyOptions {
            plan_name: options.generate.name.clone(),
            new_tip_input: options.new_tip,
            strategy: options.run.strategy,
            in_place: options.run.in_place,
        },
    )?;

    println!("{}", options.success_message);
    Ok(())
}

fn generated_plan_name(kind: &str, label: &str) -> Result<PlanName> {
    PlanName::new(format!("generated/{kind}/{label}/{}", uuid::Uuid::new_v4()))
}

fn infer_old_base_from_default_branch(git: &Git, old_tip: &str) -> Result<String> {
    if let Some(default_tip) = git.origin_default_branch_tip()? {
        return Ok(default_tip);
    }

    if let Some(default_tip) = git.local_default_branch_tip()? {
        return Ok(default_tip);
    }

    Err(Error::InvalidInvocation(format!(
        "cannot infer base branch for restack from old tip `{old_tip}`; pass --base <ref>"
    )))
}

fn excluded_target_branches(git: &Git, target: &str) -> Result<Vec<String>> {
    let mut branches = Vec::new();
    if let Some(refname) = git.symbolic_full_name(target)? {
        if let Some(branch) = refname.strip_prefix("refs/heads/") {
            branches.push(branch.to_owned());
        } else if let Some(branch) = refname.strip_prefix("refs/remotes/origin/") {
            branches.push(branch.to_owned());
        }
    } else if let Some(branch) = target.strip_prefix("origin/") {
        branches.push(branch.to_owned());
    }

    branches.sort();
    branches.dedup();
    Ok(branches)
}

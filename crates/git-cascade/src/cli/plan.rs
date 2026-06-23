use super::handle_replay_outcome;
use crate::Result;
use crate::git::Git;
use crate::model::{GitRef, Strategy};
use crate::plan::{GenerateOptions, Plan, PlanName, generate_stored_plan};
use crate::replay::state::read_state;
use crate::replay::{ReplayOptions, ReplayPauseMode, dry_run, execute};
use crate::storage::Storage;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(super) enum Command {
    /// Create a named repository-local cascade plan for an old root range.
    Create {
        /// Name to store the plan under.
        #[arg(value_name = "NAME")]
        name: PlanName,
        /// Ref used with --old-tip to compute the old range base via merge-base.
        #[arg(long, value_name = "REF")]
        old_base: GitRef,
        /// Old top of the root range before rewriting.
        #[arg(long, value_name = "REF")]
        old_tip: GitRef,
        /// Overwrite an existing plan with the same name.
        #[arg(long)]
        replace: bool,
    },
    /// Replay planned dependent branches onto a replacement root tip.
    Apply {
        /// Name of the stored plan to apply.
        #[arg(value_name = "NAME")]
        name: PlanName,
        /// Replacement ref or commit-ish for the old root tip.
        #[arg(long, value_name = "REF")]
        new_tip: GitRef,
        /// Replay strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = Strategy::MoveToCurrentTips)]
        strategy: Strategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
        /// Pause mode for replay.
        #[arg(long = "pause-at", value_enum, value_name = "MODE", default_value_t = ReplayPauseMode::Never)]
        pause_at: ReplayPauseMode,
    },
    /// List stored repository-local cascade plans by name.
    List,
    /// Print a stored plan by name.
    Show {
        /// Name of the stored plan to print.
        #[arg(value_name = "NAME")]
        name: PlanName,
    },
    /// Delete a stored plan by name.
    Remove {
        /// Name of the stored plan to delete.
        #[arg(value_name = "NAME")]
        name: PlanName,
    },
}

pub(super) fn run(command: Command) -> Result<()> {
    match command {
        Command::Create {
            name,
            old_base,
            old_tip,
            replace,
        } => create(name, old_base, old_tip, replace),
        Command::Apply {
            name,
            new_tip,
            strategy,
            dry_run,
            in_place,
            pause_at,
        } => apply(name, new_tip, strategy, dry_run, in_place, pause_at),
        Command::List => list(),
        Command::Show { name } => show(&name),
        Command::Remove { name } => remove(name),
    }
}

fn create(name: PlanName, old_base: GitRef, old_tip: GitRef, replace: bool) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    generate_stored_plan(
        &git,
        &storage,
        &GenerateOptions {
            name: name.clone(),
            old_base,
            old_tip,
            excluded_branches: Vec::new(),
        },
        replace,
    )?;
    println!("created plan `{name}`");

    Ok(())
}

fn apply(
    name: PlanName,
    new_tip: GitRef,
    strategy: Strategy,
    is_dry_run: bool,
    in_place: bool,
    replay_mode: ReplayPauseMode,
) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let plan = Plan::from_yaml(&storage.read_plan(&name)?)?;
    let options = ReplayOptions {
        plan_name: name,
        new_tip_input: new_tip,
        strategy,
        in_place,
        replay_mode,
    };

    if is_dry_run {
        print!("{}", dry_run(&git, &storage, &plan, options)?);
    } else {
        handle_replay_outcome(
            execute(&git, &storage, &plan, options)?,
            "applied cascade plan",
        )?;
    }

    Ok(())
}

fn list() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;

    for name in storage.list_plan_names()? {
        println!("{name}");
    }

    Ok(())
}

fn show(name: &PlanName) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    print!("{}", storage.read_plan(name)?);

    Ok(())
}

fn remove(name: PlanName) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    if let Some(state) = read_state(&storage)?
        && state.plan_name == name
    {
        return Err(crate::Error::InvalidInvocation(format!(
            "plan `{name}` is referenced by the active cascade operation; run `git cascade continue` or `git cascade abort` first"
        )));
    }

    // Surface a clear error when the plan does not exist.
    storage.read_plan(&name)?;
    storage.delete_plan(name.clone())?;
    println!("removed plan `{name}`");

    Ok(())
}

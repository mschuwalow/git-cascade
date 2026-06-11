mod high_level;
mod landed;
mod plan;
mod status;

use crate::Result;
use crate::apply::{abort as abort_apply, continue_apply};
use crate::git::Git;
use crate::state::{BaseStrategy, MergeStrategy};
use crate::storage::Storage;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(name = "git-cascade")]
#[command(about = "Plan and apply cascade rebases across dependent Git branches")]
pub struct Cli {
    /// Command to run.
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage stored repository-local cascade plans.
    Plan {
        /// Plan command to run.
        #[command(subcommand)]
        command: plan::Command,
    },
    /// Move dependents of a branch that advanced without rewriting old commits.
    Restack {
        /// Branch whose dependents should move. Defaults to the current branch.
        #[arg(value_name = "BRANCH")]
        branch: Option<String>,
        /// Base branch or ref the branch stack forked from. Defaults to the default branch.
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// How merge commits in dependent branches are reproduced.
        #[arg(long, value_enum, default_value_t = MergeStrategy::ReplayResolution)]
        merge_strategy: MergeStrategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
    },
    /// Replay dependents from an old root tip onto an arbitrary replacement tip.
    Replay {
        /// Old top of the root range before rewriting.
        #[arg(long, value_name = "REF")]
        old_tip: String,
        /// Ref used with --old-tip to compute the old range base via merge-base.
        #[arg(long, value_name = "REF")]
        old_base: String,
        /// Replacement ref or commit-ish for the old root tip.
        #[arg(long, value_name = "REF")]
        new_tip: String,
        /// Base selection strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = BaseStrategy::MoveToCurrentTips)]
        base_strategy: BaseStrategy,
        /// How merge commits in dependent branches are reproduced.
        #[arg(long, value_enum, default_value_t = MergeStrategy::ReplayResolution)]
        merge_strategy: MergeStrategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
    },
    /// Update branches after the default branch advanced.
    Sync {
        /// Base branch or ref to sync stacks onto. Defaults to the current default branch.
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// How merge commits in dependent branches are reproduced.
        #[arg(long, value_enum, default_value_t = MergeStrategy::ReplayResolution)]
        merge_strategy: MergeStrategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
    },
    /// Move dependents of a branch that landed on the default branch.
    Landed {
        /// Old branch tip or commit that landed.
        #[arg(value_name = "OLD-TIP")]
        old_tip: String,
        /// Branch or commit containing the landing. Defaults to the default branch.
        #[arg(long, value_name = "REF")]
        onto: Option<String>,
        /// Explicit old range base for fast-forward or ambiguous landings.
        #[arg(long, value_name = "REF")]
        old_base: Option<String>,
        /// How merge commits in dependent branches are reproduced.
        #[arg(long, value_enum, default_value_t = MergeStrategy::ReplayResolution)]
        merge_strategy: MergeStrategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
    },
    /// Show the active cascade operation, if any.
    Status,
    /// Abort the active cascade operation and clean temporary state.
    Abort,
    /// Continue an active cascade operation after resolving conflicts.
    Continue,
    /// Generate shell completion scripts.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: Shell,
    },
}

pub fn run() -> ExitCode {
    match run_from(std::env::args_os()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

pub fn run_from<I, T>(args: I) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = Cli::parse_from(args);

    match cli.command {
        Command::Plan { command } => plan::run(command),
        Command::Restack {
            branch,
            base,
            merge_strategy,
            dry_run,
            in_place,
        } => high_level::restack(
            branch,
            base,
            high_level::RunOptions::move_to_current_tips(merge_strategy, dry_run, in_place),
        ),
        Command::Replay {
            old_tip,
            old_base,
            new_tip,
            base_strategy,
            merge_strategy,
            dry_run,
            in_place,
        } => high_level::replay(
            &old_tip,
            &old_base,
            &new_tip,
            high_level::RunOptions {
                base_strategy,
                merge_strategy,
                is_dry_run: dry_run,
                in_place,
            },
        ),
        Command::Sync {
            base,
            merge_strategy,
            dry_run,
            in_place,
        } => high_level::sync(
            base,
            high_level::RunOptions::move_to_current_tips(merge_strategy, dry_run, in_place),
        ),
        Command::Landed {
            old_tip,
            onto,
            old_base,
            merge_strategy,
            dry_run,
            in_place,
        } => high_level::landed(
            &old_tip,
            onto,
            old_base,
            high_level::RunOptions::move_to_current_tips(merge_strategy, dry_run, in_place),
        ),
        Command::Status => status::status(),
        Command::Abort => abort(),
        Command::Continue => continue_operation(),
        Command::Completions { shell } => completions(shell),
    }
}

fn completions(shell: Shell) -> Result<()> {
    let mut command = Cli::command();
    generate(shell, &mut command, "git-cascade", &mut std::io::stdout());

    Ok(())
}

fn continue_operation() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    continue_apply(&git, &storage)?;
    println!("continued cascade operation");

    Ok(())
}

fn abort() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    abort_apply(&git, &storage)?;
    println!("aborted cascade operation");

    Ok(())
}

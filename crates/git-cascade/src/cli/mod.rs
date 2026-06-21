mod high_level;
mod landed;
mod plan;
mod status;

use crate::Result;
use crate::git::Git;
use crate::replay::{ReplayOutcome, abort as abort_apply, continue_apply};
use crate::state::{PausedState, Strategy};
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
        /// Replay strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = Strategy::MoveToCurrentTips)]
        strategy: Strategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
        /// Stop at child replay bases and branch ends so checks and fixes can be committed manually.
        #[arg(long)]
        pause_at_checkpoints: bool,
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
        /// Replay strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = Strategy::MoveToCurrentTips)]
        strategy: Strategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
        /// Stop at child replay bases and branch ends so checks and fixes can be committed manually.
        #[arg(long)]
        pause_at_checkpoints: bool,
    },
    /// Update branches after the default branch advanced.
    Sync {
        /// Base branch or ref to sync stacks onto. Defaults to the current default branch.
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Replay strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = Strategy::MoveToCurrentTips)]
        strategy: Strategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
        /// Stop at child replay bases and branch ends so checks and fixes can be committed manually.
        #[arg(long)]
        pause_at_checkpoints: bool,
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
        /// Replay strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = Strategy::MoveToCurrentTips)]
        strategy: Strategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
        /// Stop at child replay bases and branch ends so checks and fixes can be committed manually.
        #[arg(long)]
        pause_at_checkpoints: bool,
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
            strategy,
            dry_run,
            in_place,
            pause_at_checkpoints,
        } => high_level::restack(
            branch,
            base,
            high_level::RunOptions {
                strategy,
                is_dry_run: dry_run,
                in_place,
                pause_at_checkpoints,
            },
        ),
        Command::Replay {
            old_tip,
            old_base,
            new_tip,
            strategy,
            dry_run,
            in_place,
            pause_at_checkpoints,
        } => high_level::replay(
            &old_tip,
            &old_base,
            &new_tip,
            high_level::RunOptions {
                strategy,
                is_dry_run: dry_run,
                in_place,
                pause_at_checkpoints,
            },
        ),
        Command::Sync {
            base,
            strategy,
            dry_run,
            in_place,
            pause_at_checkpoints,
        } => high_level::sync(
            base,
            high_level::RunOptions {
                strategy,
                is_dry_run: dry_run,
                in_place,
                pause_at_checkpoints,
            },
        ),
        Command::Landed {
            old_tip,
            onto,
            old_base,
            strategy,
            dry_run,
            in_place,
            pause_at_checkpoints,
        } => high_level::landed(
            &old_tip,
            onto,
            old_base,
            high_level::RunOptions {
                strategy,
                is_dry_run: dry_run,
                in_place,
                pause_at_checkpoints,
            },
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
    let outcome = continue_apply(&git, &storage)?;
    handle_apply_outcome(outcome, "continued cascade operation")
}

pub(super) fn handle_apply_outcome(outcome: ReplayOutcome, success_message: &str) -> Result<()> {
    match outcome {
        ReplayOutcome::Complete => println!("{success_message}"),
        ReplayOutcome::Paused { paused } => print_paused_message(&paused),
        ReplayOutcome::Conflict { current, message } => {
            print_conflict_message(
                &current.branch,
                &current.commit,
                &current.worktree,
                &message,
            );
        }
    }

    Ok(())
}

fn print_conflict_message(branch: &str, commit: &str, worktree: &str, message: &str) {
    println!(
        "stopped on conflict while replaying branch `{branch}` commit `{commit}` in worktree {worktree}: {message}\n\nResolve the conflicts in that worktree, stage the resolved files with `git -C {worktree} add <files>`, then run `git cascade continue`. Do not run `git -C {worktree} cherry-pick --continue` manually; git-cascade will do that after checking its recovery state."
    );
}

pub(super) fn print_paused_message(paused: &PausedState) {
    match paused {
        PausedState::BranchEnd {
            branch, worktree, ..
        } => println!(
            "paused after branch `{branch}`; run checks in {worktree}, commit any fixes, then run `git cascade continue`"
        ),
        PausedState::ChildBase {
            branch,
            commit,
            worktree,
            ..
        } => println!(
            "paused at child base `{commit}` on branch `{branch}`; run checks in {worktree}, commit any fixes, then run `git cascade continue`"
        ),
    }
}

fn abort() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    abort_apply(&git, &storage)?;
    println!("aborted cascade operation");

    Ok(())
}

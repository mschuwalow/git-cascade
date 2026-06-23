mod high_level;
mod landed;
mod plan;
mod status;

use crate::Result;
use crate::git::Git;
use crate::model::{BranchName, GitRef, Strategy};
use crate::replay::{
    AbortOutcome, PausedState, ReplayOutcome, ReplayPauseLocation, abort as abort_apply,
    continue_replay,
};
use crate::storage::Storage;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use std::collections::BTreeSet;
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
        branch: Option<BranchName>,
        /// Base branch or ref the branch stack forked from. Defaults to the default branch.
        #[arg(long, value_name = "REF")]
        base: Option<GitRef>,
        /// Replay strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = Strategy::MoveToCurrentTips)]
        strategy: Strategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
        /// Replay locations to pause at. May be repeated or comma-separated.
        #[arg(
            long = "pause-at",
            value_enum,
            value_name = "LOCATION",
            value_delimiter = ','
        )]
        pause_at: Vec<ReplayPauseLocation>,
    },
    /// Replay dependents from an old root tip onto an arbitrary replacement tip.
    Replay {
        /// Old top of the root range before rewriting.
        #[arg(long, value_name = "REF")]
        old_tip: GitRef,
        /// Ref used with --old-tip to compute the old range base via merge-base.
        #[arg(long, value_name = "REF")]
        old_base: GitRef,
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
        /// Replay locations to pause at. May be repeated or comma-separated.
        #[arg(
            long = "pause-at",
            value_enum,
            value_name = "LOCATION",
            value_delimiter = ','
        )]
        pause_at: Vec<ReplayPauseLocation>,
    },
    /// Update branches after the default branch advanced.
    Sync {
        /// Base branch or ref to sync stacks onto. Defaults to the current default branch.
        #[arg(long, value_name = "REF")]
        base: Option<GitRef>,
        /// Oldest local branch to include. Defaults to the oldest inferred local fork point.
        #[arg(long, value_name = "REF")]
        oldest_branch: Option<GitRef>,
        /// Replay strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = Strategy::MoveToCurrentTips)]
        strategy: Strategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
        /// Replay locations to pause at. May be repeated or comma-separated.
        #[arg(
            long = "pause-at",
            value_enum,
            value_name = "LOCATION",
            value_delimiter = ','
        )]
        pause_at: Vec<ReplayPauseLocation>,
    },
    /// Move dependents of a branch that landed on the default branch.
    Landed {
        /// Old branch tip or commit that landed.
        #[arg(value_name = "OLD-TIP")]
        old_tip: GitRef,
        /// Branch or commit containing the landing. Defaults to the default branch.
        #[arg(long, value_name = "REF")]
        onto: Option<GitRef>,
        /// Explicit old range base for fast-forward or ambiguous landings.
        #[arg(long, value_name = "REF")]
        old_base: Option<GitRef>,
        /// Replay strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = Strategy::MoveToCurrentTips)]
        strategy: Strategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
        /// Replay locations to pause at. May be repeated or comma-separated.
        #[arg(
            long = "pause-at",
            value_enum,
            value_name = "LOCATION",
            value_delimiter = ','
        )]
        pause_at: Vec<ReplayPauseLocation>,
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
            pause_at,
        } => high_level::restack(
            branch,
            base,
            high_level::RunOptions {
                strategy,
                is_dry_run: dry_run,
                in_place,
                pause_at: pause_locations(pause_at),
            },
        ),
        Command::Replay {
            old_tip,
            old_base,
            new_tip,
            strategy,
            dry_run,
            in_place,
            pause_at,
        } => high_level::replay(
            old_tip,
            old_base,
            new_tip,
            high_level::RunOptions {
                strategy,
                is_dry_run: dry_run,
                in_place,
                pause_at: pause_locations(pause_at),
            },
        ),
        Command::Sync {
            base,
            oldest_branch,
            strategy,
            dry_run,
            in_place,
            pause_at,
        } => high_level::sync(
            base,
            oldest_branch,
            high_level::RunOptions {
                strategy,
                is_dry_run: dry_run,
                in_place,
                pause_at: pause_locations(pause_at),
            },
        ),
        Command::Landed {
            old_tip,
            onto,
            old_base,
            strategy,
            dry_run,
            in_place,
            pause_at,
        } => high_level::landed(
            old_tip,
            onto,
            old_base,
            high_level::RunOptions {
                strategy,
                is_dry_run: dry_run,
                in_place,
                pause_at: pause_locations(pause_at),
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

fn pause_locations(locations: Vec<ReplayPauseLocation>) -> BTreeSet<ReplayPauseLocation> {
    locations.into_iter().collect()
}

fn continue_operation() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let outcome = continue_replay(&git, &storage)?;
    handle_replay_outcome(outcome, "continued cascade operation")
}

pub(super) fn handle_replay_outcome(outcome: ReplayOutcome, success_message: &str) -> Result<()> {
    match outcome {
        ReplayOutcome::Complete => println!("{success_message}"),
        ReplayOutcome::Paused { paused } => print_paused_message(&paused),
        ReplayOutcome::Conflict {
            branch,
            commit,
            worktree,
            message,
        } => print_conflict_message(&branch, &commit, &worktree, &message),
    }

    Ok(())
}

fn print_conflict_message(
    branch: impl std::fmt::Display,
    commit: impl std::fmt::Display,
    worktree: &str,
    message: &str,
) {
    println!(
        "stopped on conflict while replaying branch `{branch}` commit `{commit}` in worktree {worktree}: {message}\n\nResolve the conflicts in that worktree, stage the resolved files with `git -C {worktree} add <files>`, then run `git cascade continue`. Do not run `git -C {worktree} cherry-pick --continue` manually; git-cascade will do that after checking its recovery state."
    );
}

pub(super) fn print_paused_message(paused: &PausedState) {
    if paused
        .reasons()
        .contains(&crate::replay::PauseReason::BranchEnd)
    {
        println!(
            "paused after branch `{}`; run checks in {}, commit fixes or rewrite this branch while preserving child replay bases, then run `git cascade continue`\nstop reasons: {}",
            paused.branch,
            paused.worktree,
            paused.reason_list(),
        );
    } else if let crate::replay::PausedKind::MidBranch { replay } = &paused.kind {
        let commit = replay
            .last_replayed_commit
            .as_ref()
            .expect("mid-branch pause has current commit");
        let kind = if paused
            .reasons()
            .contains(&crate::replay::PauseReason::ChildBase)
        {
            "child base commit"
        } else {
            "commit"
        };
        println!(
            "paused at {kind} `{commit}` on branch `{}`; run checks in {}, commit any fixes, then run `git cascade continue`\nstop reasons: {}",
            paused.branch,
            paused.worktree,
            paused.reason_list(),
        );
    }
}

fn abort() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    match abort_apply(&git, &storage)? {
        AbortOutcome::Aborted => println!("aborted cascade operation"),
        AbortOutcome::CompletedCleanup => println!("completed cascade cleanup"),
    }

    Ok(())
}

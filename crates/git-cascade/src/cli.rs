use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};

use crate::Result;
use crate::apply::{ApplyOptions, DryRunOptions, continue_apply, dry_run, execute};
use crate::git::Git;
use crate::plan_generate::{GenerateOptions, generate_anchor_keyed_plan};
use crate::recovery;
use crate::state::Strategy;
use crate::storage::{PlanKey, Storage};

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
    /// Create a repository-local cascade plan rooted at an old anchor ref.
    Plan {
        /// Old anchor ref or commit-ish to snapshot before rewriting dependents.
        #[arg(long, value_name = "REF")]
        anchor: String,
        /// Overwrite an existing plan for the same anchor key.
        #[arg(long)]
        replace: bool,
    },
    /// Replay planned dependent branches onto a replacement anchor.
    Apply {
        /// Old anchor key used when the plan was created.
        #[arg(long, value_name = "REF")]
        old_anchor: String,
        /// Replacement ref or commit-ish for the old anchor boundary.
        #[arg(long, value_name = "REF")]
        new_anchor: String,
        /// Replay strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = Strategy::PreserveForkPoints)]
        strategy: Strategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
    },
    /// List stored repository-local cascade plans by anchor key.
    List,
    /// Print the stored plan for an anchor key.
    Show {
        /// Anchor key used when the plan was created.
        #[arg(long, value_name = "REF")]
        anchor: String,
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
        Command::Plan { anchor, replace } => plan(&anchor, replace),
        Command::Apply {
            old_anchor,
            new_anchor,
            strategy,
            dry_run,
        } => apply(&old_anchor, &new_anchor, strategy, dry_run),
        Command::List => list_plans(),
        Command::Show { anchor } => show_plan(&anchor),
        Command::Status => status(),
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

fn status() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    print!("{}", recovery::status(&git, &storage)?);

    Ok(())
}

fn abort() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    recovery::abort(&git, &storage)?;
    println!("aborted cascade operation");

    Ok(())
}

fn apply(old_anchor: &str, new_anchor: &str, strategy: Strategy, is_dry_run: bool) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let key = PlanKey::from_anchor(old_anchor)?;
    let plan = serde_yaml::from_str(&storage.read_plan(&key)?)?;

    if is_dry_run {
        print!(
            "{}",
            dry_run(
                &git,
                &storage,
                &plan,
                DryRunOptions {
                    new_anchor_input: new_anchor.to_owned(),
                    strategy,
                },
            )?
        );
    } else {
        execute(
            &git,
            &storage,
            &plan,
            ApplyOptions {
                plan_key: key,
                new_anchor_input: new_anchor.to_owned(),
                strategy,
            },
        )?;
        println!("applied cascade plan");
    }

    Ok(())
}

fn plan(anchor_ref: &str, replace: bool) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    generate_anchor_keyed_plan(
        &git,
        &storage,
        GenerateOptions {
            anchor_ref: anchor_ref.to_owned(),
            replace,
        },
    )?;
    println!("created plan for anchor `{anchor_ref}`");

    Ok(())
}

fn list_plans() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;

    for name in storage.list_plan_keys()? {
        println!("{name}");
    }

    Ok(())
}

fn show_plan(anchor: &str) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let key = PlanKey::from_anchor(anchor)?;
    print!("{}", storage.read_plan(&key)?);

    Ok(())
}

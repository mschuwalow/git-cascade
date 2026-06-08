use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};

use crate::Result;
use crate::apply::{
    ApplyOptions, DryRunOptions, abort as abort_apply, continue_apply, dry_run, execute,
};
use crate::git::Git;
use crate::plan_generate::{GenerateOptions, generate_named_plan};
use crate::state::Strategy;
use crate::status;
use crate::storage::{PlanName, Storage};

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
    /// Create a named repository-local cascade plan for an old root range.
    Plan {
        /// Name to store the plan under.
        #[arg(value_name = "NAME")]
        name: PlanName,
        /// Ref used with --old-tip to compute the old range base via merge-base.
        /// Inferred from default branches when omitted.
        #[arg(long, value_name = "REF")]
        old_base: Option<String>,
        /// Old top of the root range before rewriting.
        #[arg(long, value_name = "REF")]
        old_tip: String,
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
        new_tip: String,
        /// Replay strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = Strategy::PreserveForkPoints)]
        strategy: Strategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
    },
    /// List stored repository-local cascade plans by name.
    List,
    /// Print a stored plan by name.
    Show {
        /// Name of the stored plan to print.
        #[arg(value_name = "NAME")]
        name: PlanName,
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
        Command::Plan {
            name,
            old_base,
            old_tip,
            replace,
        } => plan(name, old_base.as_deref(), &old_tip, replace),
        Command::Apply {
            name,
            new_tip,
            strategy,
            dry_run,
            in_place,
        } => apply(name, &new_tip, strategy, dry_run, in_place),
        Command::List => list_plans(),
        Command::Show { name } => show_plan(&name),
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
    print!("{}", status::status(&storage)?);

    Ok(())
}

fn abort() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    abort_apply(&git, &storage)?;
    println!("aborted cascade operation");

    Ok(())
}

fn apply(
    name: PlanName,
    new_tip: &str,
    strategy: Strategy,
    is_dry_run: bool,
    in_place: bool,
) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let plan = serde_yaml::from_str(&storage.read_plan(&name)?)?;

    if is_dry_run {
        print!(
            "{}",
            dry_run(
                &git,
                &storage,
                &plan,
                DryRunOptions {
                    plan_name: name.clone(),
                    new_tip_input: new_tip.to_owned(),
                    strategy,
                    in_place,
                },
            )?
        );
    } else {
        execute(
            &git,
            &storage,
            &plan,
            ApplyOptions {
                plan_name: name,
                new_tip_input: new_tip.to_owned(),
                strategy,
                in_place,
            },
        )?;
        println!("applied cascade plan");
    }

    Ok(())
}

fn plan(name: PlanName, old_base: Option<&str>, old_tip: &str, replace: bool) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    generate_named_plan(
        &git,
        &storage,
        GenerateOptions {
            name: name.clone(),
            old_base: old_base.map(str::to_owned),
            old_tip: old_tip.to_owned(),
            replace,
        },
    )?;
    println!("created plan `{name}`");

    Ok(())
}

fn list_plans() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;

    for name in storage.list_plan_names()? {
        println!("{name}");
    }

    Ok(())
}

fn show_plan(name: &PlanName) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    print!("{}", storage.read_plan(name)?);

    Ok(())
}

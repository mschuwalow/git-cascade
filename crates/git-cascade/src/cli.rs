use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::Result;
use crate::apply::{ApplyOptions, DryRunOptions, continue_apply, dry_run, execute};
use crate::git::Git;
use crate::plan_generate::{GenerateOptions, generate_named_plan};
use crate::recovery;
use crate::state::Strategy;
use crate::storage::{PlanKey, Storage};

#[derive(Debug, Parser)]
#[command(name = "git-cascade")]
#[command(about = "Plan and apply cascade rebases across dependent Git branches")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Plan {
        #[arg(long)]
        anchor: String,
        #[arg(long)]
        replace: bool,
    },
    Apply {
        #[arg(long)]
        anchor: String,
        #[arg(long)]
        new_anchor: String,
        #[arg(long, value_enum, default_value_t = Strategy::PreserveForkPoints)]
        strategy: Strategy,
        #[arg(long)]
        dry_run: bool,
    },
    List,
    Show {
        #[arg(long)]
        anchor: String,
    },
    Status,
    Abort,
    Continue,
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
            anchor,
            new_anchor,
            strategy,
            dry_run,
        } => apply(&anchor, &new_anchor, strategy, dry_run),
        Command::List => list_plans(),
        Command::Show { anchor } => show_plan(&anchor),
        Command::Status => status(),
        Command::Abort => abort(),
        Command::Continue => continue_operation(),
    }
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

fn apply(anchor: &str, new_anchor: &str, strategy: Strategy, is_dry_run: bool) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let key = PlanKey::from_anchor(anchor)?;
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

fn plan(anchor_branch: &str, replace: bool) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    generate_named_plan(
        &git,
        &storage,
        GenerateOptions {
            anchor_branch: anchor_branch.to_owned(),
            replace,
        },
    )?;
    println!("created plan for anchor `{anchor_branch}`");

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

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::Result;
use crate::apply::{ApplyOptions, DryRunOptions, continue_apply, dry_run, execute};
use crate::git::Git;
use crate::plan_generate::{GenerateOptions, generate_named_plan};
use crate::recovery;
use crate::storage::{PlanName, Storage};

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
        anchor_branch: String,
        #[arg(long)]
        name: PlanName,
        #[arg(long)]
        main: Option<String>,
        #[arg(long)]
        replace: bool,
    },
    Apply {
        #[arg(long)]
        name: PlanName,
        #[arg(long)]
        new_anchor: String,
        #[arg(long)]
        move_to_heads: bool,
        #[arg(long)]
        dry_run: bool,
    },
    List,
    Show {
        #[arg(long)]
        name: PlanName,
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
        Command::Plan {
            anchor_branch,
            name,
            main,
            replace,
        } => plan(&anchor_branch, name, main.as_deref(), replace),
        Command::Apply {
            name,
            new_anchor,
            move_to_heads,
            dry_run,
        } => apply(name, &new_anchor, move_to_heads, dry_run),
        Command::List => list_plans(),
        Command::Show { name } => show_plan(&name),
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
    print!("{}", recovery::status(&storage)?);

    Ok(())
}

fn abort() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    recovery::abort(&git, &storage)?;
    println!("aborted cascade operation");

    Ok(())
}

fn apply(name: PlanName, new_anchor: &str, move_to_heads: bool, is_dry_run: bool) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let plan = serde_yaml::from_str(&storage.read_named_plan(&name)?)?;

    if is_dry_run {
        print!(
            "{}",
            dry_run(
                &git,
                &storage,
                &plan,
                DryRunOptions {
                    new_anchor_input: new_anchor.to_owned(),
                    move_to_heads,
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
                new_anchor_input: new_anchor.to_owned(),
                move_to_heads,
            },
        )?;
        println!("applied cascade plan");
    }

    Ok(())
}

fn plan(anchor_branch: &str, name: PlanName, main: Option<&str>, replace: bool) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let display_name = name.to_string();
    generate_named_plan(
        &git,
        &storage,
        GenerateOptions {
            anchor_branch: anchor_branch.to_owned(),
            name,
            replace,
            main: main.map(str::to_owned),
        },
    )?;
    println!("created plan `{display_name}`");

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
    print!("{}", storage.read_named_plan(name)?);

    Ok(())
}

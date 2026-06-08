use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::git::Git;
use crate::storage::Storage;
use crate::{Error, Result};

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
        name: String,
        #[arg(long)]
        replace: bool,
    },
    Apply {
        #[arg(long, conflicts_with = "plan")]
        name: Option<String>,
        #[arg(long, conflicts_with = "name")]
        plan: Option<std::path::PathBuf>,
        #[arg(long)]
        new_anchor: String,
        #[arg(long)]
        move_to_heads: bool,
    },
    List,
    Show {
        #[arg(long)]
        name: String,
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
            anchor_branch: _,
            name: _,
            replace: _,
        } => Err(Error::Unsupported(
            "plan generation is not implemented yet".to_owned(),
        )),
        Command::Apply {
            name: _,
            plan: _,
            new_anchor: _,
            move_to_heads: _,
        } => Err(Error::Unsupported(
            "plan application is not implemented yet".to_owned(),
        )),
        Command::List => list_plans(),
        Command::Show { name } => show_plan(&name),
    }
}

fn list_plans() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;

    for name in storage.list_plan_names()? {
        println!("{name}");
    }

    Ok(())
}

fn show_plan(name: &str) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    print!("{}", storage.read_named_plan(name)?);

    Ok(())
}

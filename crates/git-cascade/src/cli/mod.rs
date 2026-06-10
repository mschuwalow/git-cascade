mod landed;
mod status;

use crate::apply::{
    ApplyOptions, DryRunOptions, abort as abort_apply, continue_apply, dry_run, execute,
};
use crate::git::Git;
use crate::plan::PlanName;
use crate::plan::{GenerateOptions, generate_plan, generate_stored_plan};
use crate::state::Strategy;
use crate::storage::Storage;
use crate::{Error, Result};
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
    /// Create a named repository-local cascade plan for an old root range.
    Plan {
        /// Name to store the plan under.
        #[arg(value_name = "NAME")]
        name: PlanName,
        /// Ref used with --old-tip to compute the old range base via merge-base.
        #[arg(long, value_name = "REF")]
        old_base: String,
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
    /// Move dependents of a branch that advanced without rewriting old commits.
    Restack {
        /// Branch whose dependents should move. Defaults to the current branch.
        #[arg(value_name = "BRANCH")]
        branch: Option<String>,
        /// Replacement ref for the branch tip. Defaults to BRANCH.
        #[arg(long, value_name = "REF")]
        onto: Option<String>,
        /// Replay strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = Strategy::MoveToCurrentTips)]
        strategy: Strategy,
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
        /// Replay strategy for dependent branches.
        #[arg(long, value_enum, default_value_t = Strategy::MoveToCurrentTips)]
        strategy: Strategy,
        /// Print the Git operations without mutating refs, worktrees, or state.
        #[arg(long)]
        dry_run: bool,
        /// Replay in the current worktree instead of a temporary worktree.
        #[arg(long)]
        in_place: bool,
    },
    /// Update branches after the default branch advanced.
    Sync {
        /// Branch or commit to replay onto. Defaults to the current default branch.
        #[arg(long, value_name = "REF")]
        onto: Option<String>,
        /// Previous tip of --onto before it advanced. Defaults to <onto>@{1}.
        #[arg(long, value_name = "REF")]
        old_tip: Option<String>,
        /// Explicit old range base. Defaults to <old-tip>^.
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
        } => plan(name, &old_base, &old_tip, replace),
        Command::Apply {
            name,
            new_tip,
            strategy,
            dry_run,
            in_place,
        } => apply(name, &new_tip, strategy, dry_run, in_place),
        Command::Restack {
            branch,
            onto,
            strategy,
            dry_run,
            in_place,
        } => restack(branch, onto, strategy, dry_run, in_place),
        Command::Replay {
            old_tip,
            old_base,
            new_tip,
            strategy,
            dry_run,
            in_place,
        } => replay(&old_tip, &old_base, &new_tip, strategy, dry_run, in_place),
        Command::Sync {
            onto,
            old_tip,
            old_base,
            strategy,
            dry_run,
            in_place,
        } => sync(onto, old_tip, old_base, strategy, dry_run, in_place),
        Command::Landed {
            old_tip,
            onto,
            old_base,
            strategy,
            dry_run,
            in_place,
        } => landed(&old_tip, onto, old_base, strategy, dry_run, in_place),
        Command::List => list_plans(),
        Command::Show { name } => show_plan(&name),
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

fn plan(name: PlanName, old_base: &str, old_tip: &str, replace: bool) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    generate_stored_plan(
        &git,
        &storage,
        &GenerateOptions {
            name: name.clone(),
            old_base: old_base.to_owned(),
            old_tip: old_tip.to_owned(),
            excluded_branches: Vec::new(),
        },
        replace,
    )?;
    println!("created plan `{name}`");

    Ok(())
}

fn restack(
    branch: Option<String>,
    onto: Option<String>,
    strategy: Strategy,
    is_dry_run: bool,
    in_place: bool,
) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let branch = branch.or(git.current_branch()?).ok_or_else(|| {
        Error::InvalidInvocation("restack needs a branch when HEAD is detached".to_owned())
    })?;
    let new_tip = onto.unwrap_or_else(|| branch.clone());
    let old_base = infer_old_base_from_default_branch(&git, "restack", &branch)?;
    let excluded_branches = excluded_target_branches(&git, &new_tip)?;
    let plan_name = generated_plan_name("restack", &branch)?;

    generate_and_apply(GeneratedApply {
        git: &git,
        storage: &storage,
        generate: GenerateOptions {
            name: plan_name,
            old_base,
            old_tip: branch,
            excluded_branches,
        },
        new_tip,
        strategy,
        is_dry_run,
        in_place,
        success_message: "restacked dependent branches",
    })
}

fn replay(
    old_tip: &str,
    old_base: &str,
    new_tip: &str,
    strategy: Strategy,
    is_dry_run: bool,
    in_place: bool,
) -> Result<()> {
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
        strategy,
        is_dry_run,
        in_place,
        success_message: "replayed dependent branches",
    })
}

fn sync(
    onto: Option<String>,
    old_tip: Option<String>,
    old_base: Option<String>,
    strategy: Strategy,
    is_dry_run: bool,
    in_place: bool,
) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let onto = onto
        .or(current_local_default_branch(&git)?)
        .or(git.default_branch_ref()?)
        .ok_or_else(|| {
            Error::InvalidInvocation(
                "sync needs --onto <ref> when no default branch exists".to_owned(),
            )
        })?;
    let old_tip = old_tip.unwrap_or_else(|| format!("{onto}@{{1}}"));
    let old_base = old_base.unwrap_or_else(|| format!("{old_tip}^"));
    let excluded_branches = excluded_target_branches(&git, &onto)?;
    let plan_name = generated_plan_name("sync", &onto)?;

    generate_and_apply(GeneratedApply {
        git: &git,
        storage: &storage,
        generate: GenerateOptions {
            name: plan_name,
            old_base,
            old_tip,
            excluded_branches,
        },
        new_tip: onto,
        strategy,
        is_dry_run,
        in_place,
        success_message: "synced dependent branches",
    })
}

fn current_local_default_branch(git: &Git) -> Result<Option<String>> {
    Ok(git
        .current_branch()?
        .filter(|branch| matches!(branch.as_str(), "main" | "master")))
}

fn landed(
    old_tip: &str,
    onto: Option<String>,
    old_base: Option<String>,
    strategy: Strategy,
    is_dry_run: bool,
    in_place: bool,
) -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    let onto = onto.or(git.default_branch_ref()?).ok_or_else(|| {
        Error::InvalidInvocation(
            "landed needs --onto <ref> when no default branch exists".to_owned(),
        )
    })?;
    let inference = landed::infer_range(&git, old_tip, &onto, old_base)?;
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
        strategy,
        is_dry_run,
        in_place,
        success_message: "updated dependents of landed branch",
    })
}

struct GeneratedApply<'a> {
    git: &'a Git,
    storage: &'a Storage,
    generate: GenerateOptions,
    new_tip: String,
    strategy: Strategy,
    is_dry_run: bool,
    in_place: bool,
    success_message: &'static str,
}

fn generate_and_apply(options: GeneratedApply<'_>) -> Result<()> {
    if options.is_dry_run {
        let plan = generate_plan(options.git, &options.generate)?;
        print!(
            "{}",
            dry_run(
                options.git,
                options.storage,
                &plan,
                DryRunOptions {
                    plan_name: options.generate.name,
                    new_tip_input: options.new_tip,
                    strategy: options.strategy,
                    in_place: options.in_place,
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
            strategy: options.strategy,
            in_place: options.in_place,
        },
    )?;

    println!("{}", options.success_message);
    Ok(())
}

fn generated_plan_name(kind: &str, label: &str) -> Result<PlanName> {
    PlanName::new(format!("generated/{kind}/{label}/{}", uuid::Uuid::new_v4()))
}

fn infer_old_base_from_default_branch(git: &Git, name: &str, old_tip: &str) -> Result<String> {
    if let Some(default_tip) = git.origin_default_branch_tip()? {
        return Ok(default_tip);
    }

    if let Some(default_tip) = git.local_default_branch_tip()? {
        return Ok(default_tip);
    }

    Err(Error::CannotInferOldBase {
        name: name.to_owned(),
        old_tip: old_tip.to_owned(),
    })
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

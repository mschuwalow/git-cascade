use crate::plan::{PlanCommit, PlanId, PlanName};
use crate::storage::Storage;
use crate::{Error, Result};
use clap::ValueEnum;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyState {
    pub version: u32,
    pub phase: Phase,
    pub plan_name: PlanName,
    pub plan_id: PlanId,
    pub started_at: String,
    pub updated_at: String,
    pub pid: u32,
    pub new_tip: String,
    pub strategy: Strategy,
    pub replay_mode: ReplayMode,
    pub worktree: WorktreeState,
    pub completed_temp_refs: Vec<String>,
    pub branch_tips: BTreeMap<String, String>,
    pub extra_commits: BTreeMap<String, Vec<PlanCommit>>,
    pub mappings: BTreeMap<String, String>,
    pub pending_branches: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Phase {
    Replay {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current: Option<CurrentState>,
    },
    Conflict {
        current: CurrentState,
        message: String,
    },
    ContinueAfterConflict {
        current: CurrentState,
    },
    Paused {
        paused: PausedState,
    },
    ContinueAfterPause {
        paused: PausedState,
    },
    FinalUpdate,
    Deleting {
        delete_plan: bool,
    },
}

impl Phase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Replay { .. } => "replay",
            Self::Conflict { .. } => "conflict",
            Self::ContinueAfterConflict { .. } => "continue_after_conflict",
            Self::Paused { .. } => "paused",
            Self::ContinueAfterPause { .. } => "continue_after_pause",
            Self::FinalUpdate => "final_update",
            Self::Deleting { .. } => "deleting",
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    /// Preserve old fork points between dependent branches.
    PreserveForkPoints,
    /// Replay each dependent branch onto its parent's rewritten planned tip.
    MoveToPlannedTips,
    /// Replay each dependent branch onto its parent's rewritten apply-time tip.
    MoveToCurrentTips,
}

impl Strategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreserveForkPoints => "preserve-fork-points",
            Self::MoveToPlannedTips => "move-to-planned-tips",
            Self::MoveToCurrentTips => "move-to-current-tips",
        }
    }
}

impl std::fmt::Display for Strategy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReplayMode {
    #[default]
    RunToCompletion,
    PauseAtCheckpoints,
}

impl ReplayMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RunToCompletion => "run-to-completion",
            Self::PauseAtCheckpoints => "pause-at-checkpoints",
        }
    }

    pub fn pauses_at_checkpoints(self) -> bool {
        self == Self::PauseAtCheckpoints
    }
}

impl std::fmt::Display for ReplayMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CurrentState {
    pub branch: String,
    pub commit: String,
    pub worktree: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PausedState {
    BranchEnd {
        branch: String,
        rewritten_tip: String,
        temp_ref: String,
        mapped_commit: String,
        worktree: String,
    },
    ChildBase {
        branch: String,
        commit: String,
        rewritten_tip: String,
        worktree: String,
    },
}

impl PausedState {
    pub fn branch(&self) -> &str {
        match self {
            Self::BranchEnd { branch, .. } | Self::ChildBase { branch, .. } => branch,
        }
    }

    pub fn rewritten_tip(&self) -> &str {
        match self {
            Self::BranchEnd { rewritten_tip, .. } | Self::ChildBase { rewritten_tip, .. } => {
                rewritten_tip
            }
        }
    }

    pub fn worktree(&self) -> &str {
        match self {
            Self::BranchEnd { worktree, .. } | Self::ChildBase { worktree, .. } => worktree,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum WorktreeState {
    Temporary { path: String },
    InPlace { path: String, restore: RestoreState },
}

impl WorktreeState {
    pub fn mode(&self) -> &'static str {
        match self {
            Self::Temporary { .. } => "temporary",
            Self::InPlace { .. } => "in-place",
        }
    }

    pub fn path(&self) -> &str {
        match self {
            Self::Temporary { path } | Self::InPlace { path, .. } => path,
        }
    }

    pub fn is_temporary(&self) -> bool {
        matches!(self, Self::Temporary { .. })
    }

    pub fn is_in_place(&self) -> bool {
        matches!(self, Self::InPlace { .. })
    }
}

impl std::fmt::Display for WorktreeState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.mode())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RestoreState {
    Branch { name: String, head: String },
    Detached { head: String },
}

pub struct StateFile {
    path: PathBuf,
    lock_file: File,
}

fn acquire_lock(lock_file: &File, lock_path: &Path) -> Result<()> {
    lock_file.try_lock_exclusive().map_err(|source| {
        if source.kind() == fs2::lock_contended_error().kind() {
            Error::InvalidInvocation(
                "another git-cascade command is currently running in this repository".to_owned(),
            )
        } else {
            Error::IoWithPath {
                path: lock_path.to_owned(),
                source,
            }
        }
    })
}

impl StateFile {
    pub fn create(storage: &Storage, state: &ApplyState) -> Result<Self> {
        storage.ensure_cascade_dir()?;
        let path = storage.state_path();
        let lock_path = storage.state_lock_path();
        let lock_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| Error::IoWithPath {
                path: lock_path.clone(),
                source,
            })?;
        acquire_lock(&lock_file, &lock_path)?;
        if path.exists() {
            return Err(Error::ActiveOperation { path });
        }

        let mut state_file = Self { path, lock_file };
        state_file.write_state_without_touching_timestamp(state)?;
        Ok(state_file)
    }

    pub fn open(storage: &Storage) -> Result<Option<Self>> {
        let path = storage.state_path();
        if !path.exists() {
            return Ok(None);
        }

        let lock_path = storage.state_lock_path();
        let lock_file = match fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(Error::IoWithPath {
                    path: lock_path,
                    source,
                });
            }
        };
        acquire_lock(&lock_file, &lock_path)?;
        if !path.exists() {
            return Ok(None);
        }

        Ok(Some(Self { path, lock_file }))
    }

    pub fn read_state(&mut self) -> Result<ApplyState> {
        let content = fs::read_to_string(&self.path).map_err(|source| Error::IoWithPath {
            path: self.path.clone(),
            source,
        })?;
        Ok(serde_yaml::from_str(&content)?)
    }

    pub fn write_state(&mut self, state: &mut ApplyState) -> Result<()> {
        state.updated_at = timestamp()?;
        self.write_state_without_touching_timestamp(state)
    }

    pub fn remove(self) -> Result<()> {
        fs::remove_file(&self.path).map_err(|source| Error::IoWithPath {
            path: self.path.clone(),
            source,
        })
    }

    pub fn remove_if_exists(self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::IoWithPath {
                path: self.path.clone(),
                source,
            }),
        }
    }

    fn write_state_without_touching_timestamp(&mut self, state: &ApplyState) -> Result<()> {
        let yaml = serde_yaml::to_string(state)?;
        let temp_path = self
            .path
            .with_extension(format!("yaml.{}.tmp", std::process::id()));
        {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temp_path)
                .map_err(|source| Error::IoWithPath {
                    path: temp_path.clone(),
                    source,
                })?;
            file.write_all(yaml.as_bytes())?;
            file.sync_data()?;
        }
        fs::rename(&temp_path, &self.path).map_err(|source| Error::IoWithPath {
            path: self.path.clone(),
            source,
        })?;
        if let Some(parent) = self.path.parent()
            && let Ok(directory) = File::open(parent)
        {
            let _ = directory.sync_all();
        }
        let _ = self.lock_file.sync_data();
        Ok(())
    }
}

pub fn read_state(storage: &Storage) -> Result<Option<ApplyState>> {
    let path = storage.state_path();
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(Error::IoWithPath { path, source }),
    };

    Ok(Some(serde_yaml::from_str(&content)?))
}

pub struct InitialApplyStateInput<'a> {
    pub plan_name: &'a PlanName,
    pub plan_id: &'a PlanId,
    pub new_tip: &'a str,
    pub strategy: Strategy,
    pub replay_mode: ReplayMode,
    pub pending_branches: Vec<String>,
    pub branch_tips: BTreeMap<String, String>,
    pub extra_commits: BTreeMap<String, Vec<PlanCommit>>,
    pub mappings: BTreeMap<String, String>,
    pub worktree: WorktreeState,
}

pub fn initial_apply_state(input: InitialApplyStateInput<'_>) -> Result<ApplyState> {
    let now = timestamp()?;

    Ok(ApplyState {
        version: 1,
        phase: Phase::Replay { current: None },
        plan_name: input.plan_name.clone(),
        plan_id: *input.plan_id,
        started_at: now.clone(),
        updated_at: now,
        pid: std::process::id(),
        new_tip: input.new_tip.to_owned(),
        strategy: input.strategy,
        replay_mode: input.replay_mode,
        worktree: input.worktree,
        completed_temp_refs: Vec::new(),
        branch_tips: input.branch_tips,
        extra_commits: input.extra_commits,
        mappings: input.mappings,
        pending_branches: input.pending_branches
    })
}

fn timestamp() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| Error::Unsupported(format!("failed to format state timestamp: {error}")))
}

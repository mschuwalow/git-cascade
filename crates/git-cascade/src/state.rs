use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use clap::ValueEnum;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::storage::{PlanName, Storage};
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyState {
    pub version: u32,
    pub phase: Phase,
    pub plan_name: PlanName,
    pub plan_id: String,
    pub started_at: String,
    pub updated_at: String,
    pub pid: u32,
    pub new_tip: NewTipState,
    pub strategy: Strategy,
    pub current: Option<CurrentState>,
    pub worktree: String,
    pub completed: CompletedState,
    pub branch_tips: BTreeMap<String, String>,
    pub extra_commits: BTreeMap<String, Vec<String>>,
    pub mappings: BTreeMap<String, String>,
    pub pending: PendingState,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Replay,
    Conflict,
    FinalUpdate,
    Deleting,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Replay => "replay",
            Self::Conflict => "conflict",
            Self::FinalUpdate => "final_update",
            Self::Deleting => "deleting",
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTipState {
    pub input: String,
    pub resolved: String,
    pub input_was_ref: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentState {
    pub branch: String,
    pub commit: String,
    pub worktree: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompletedState {
    pub temp_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingState {
    pub branches: Vec<String>,
}

pub struct StateFile {
    path: PathBuf,
    file: File,
}

impl StateFile {
    pub fn create(storage: &Storage, state: &ApplyState) -> Result<Self> {
        storage.ensure_cascade_dir()?;
        let path = storage.state_path();
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::AlreadyExists {
                    Error::ActiveOperation { path: path.clone() }
                } else {
                    Error::IoWithPath {
                        path: path.clone(),
                        source,
                    }
                }
            })?;
        file.lock_exclusive()?;

        let mut state_file = Self { path, file };
        state_file.write_state_without_touching_timestamp(state)?;
        Ok(state_file)
    }

    pub fn open(storage: &Storage) -> Result<Option<Self>> {
        let path = storage.state_path();
        let file = match fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(Error::IoWithPath { path, source }),
        };
        file.lock_exclusive()?;

        Ok(Some(Self { path, file }))
    }

    pub fn read_state(&mut self) -> Result<ApplyState> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut content = String::new();
        self.file.read_to_string(&mut content)?;
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
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(yaml.as_bytes())?;
        self.file.sync_data()?;
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

pub fn require_state(storage: &Storage) -> Result<ApplyState> {
    read_state(storage)?
        .ok_or_else(|| Error::InvalidInvocation("no active cascade operation".to_owned()))
}

pub fn remove_state(storage: &Storage) -> Result<()> {
    let path = storage.state_path();
    fs::remove_file(&path).map_err(|source| Error::IoWithPath { path, source })
}

pub struct ApplyStateInput<'a> {
    pub plan_name: &'a PlanName,
    pub plan_id: &'a str,
    pub new_tip_input: &'a str,
    pub new_tip_resolved: &'a str,
    pub new_tip_input_was_ref: bool,
    pub strategy: Strategy,
    pub pending_branches: Vec<String>,
    pub branch_tips: BTreeMap<String, String>,
    pub extra_commits: BTreeMap<String, Vec<String>>,
    pub mappings: BTreeMap<String, String>,
    pub worktree: String,
}

pub fn initial_apply_state(input: ApplyStateInput<'_>) -> Result<ApplyState> {
    let now = timestamp()?;

    Ok(ApplyState {
        version: 1,
        phase: Phase::Replay,
        plan_name: input.plan_name.clone(),
        plan_id: input.plan_id.to_owned(),
        started_at: now.clone(),
        updated_at: now,
        pid: std::process::id(),
        new_tip: NewTipState {
            input: input.new_tip_input.to_owned(),
            resolved: input.new_tip_resolved.to_owned(),
            input_was_ref: input.new_tip_input_was_ref,
        },
        strategy: input.strategy,
        current: None,
        worktree: input.worktree,
        completed: CompletedState::default(),
        branch_tips: input.branch_tips,
        extra_commits: input.extra_commits,
        mappings: input.mappings,
        pending: PendingState {
            branches: input.pending_branches,
        },
    })
}

fn timestamp() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| Error::Unsupported(format!("failed to format state timestamp: {error}")))
}

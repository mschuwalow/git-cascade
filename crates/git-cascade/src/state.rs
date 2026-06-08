use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::storage::{PlanName, Storage};
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyState {
    pub version: u32,
    pub operation: String,
    pub phase: String,
    pub plan_name: Option<String>,
    pub plan_id: String,
    pub started_at: String,
    pub updated_at: String,
    pub pid: u32,
    pub new_anchor: NewAnchorState,
    pub strategy: StrategyState,
    pub current: Option<CurrentState>,
    pub completed: CompletedState,
    pub mappings: BTreeMap<String, String>,
    pub pending: PendingState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewAnchorState {
    pub input: String,
    pub resolved: String,
    pub input_was_ref: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyState {
    pub move_to_heads: bool,
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

pub struct StateLock {
    path: PathBuf,
}

impl StateLock {
    pub fn create(storage: &Storage, state: &ApplyState) -> Result<Self> {
        storage.ensure_cascade_dir()?;
        let path = storage.state_path();
        let yaml = serde_yaml::to_string(state)?;
        let mut file = fs::OpenOptions::new()
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
        file.write_all(yaml.as_bytes())?;

        Ok(Self { path })
    }

    pub fn remove(self) -> Result<()> {
        fs::remove_file(&self.path).map_err(|source| Error::IoWithPath {
            path: self.path,
            source,
        })
    }
}

pub struct ApplyStateInput<'a> {
    pub plan_name: Option<&'a PlanName>,
    pub plan_id: &'a str,
    pub new_anchor_input: &'a str,
    pub new_anchor_resolved: &'a str,
    pub new_anchor_input_was_ref: bool,
    pub move_to_heads: bool,
    pub pending_branches: Vec<String>,
    pub mappings: BTreeMap<String, String>,
}

pub fn initial_apply_state(input: ApplyStateInput<'_>) -> Result<ApplyState> {
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| {
            Error::Unsupported(format!("failed to format state timestamp: {error}"))
        })?;

    Ok(ApplyState {
        version: 1,
        operation: "apply".to_owned(),
        phase: "replay".to_owned(),
        plan_name: input.plan_name.map(ToString::to_string),
        plan_id: input.plan_id.to_owned(),
        started_at: now.clone(),
        updated_at: now,
        pid: std::process::id(),
        new_anchor: NewAnchorState {
            input: input.new_anchor_input.to_owned(),
            resolved: input.new_anchor_resolved.to_owned(),
            input_was_ref: input.new_anchor_input_was_ref,
        },
        strategy: StrategyState {
            move_to_heads: input.move_to_heads,
        },
        current: None,
        completed: CompletedState::default(),
        mappings: input.mappings,
        pending: PendingState {
            branches: input.pending_branches,
        },
    })
}

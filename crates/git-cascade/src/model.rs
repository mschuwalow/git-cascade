use clap::ValueEnum;
use serde::{Deserialize, Serialize};

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

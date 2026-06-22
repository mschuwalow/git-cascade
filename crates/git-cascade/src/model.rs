use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::str::FromStr;

macro_rules! string_newtype {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl FromStr for $name {
            type Err = Infallible;

            fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
                Ok(Self::new(value))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

// Name of a local Git branch, without `refs/heads/`.
string_newtype!(BranchName);
// Object id for a resolved commit.
string_newtype!(CommitId);
// User-supplied Git revision or ref expression that still needs resolving.
string_newtype!(GitRef);

impl From<CommitId> for GitRef {
    fn from(commit: CommitId) -> Self {
        Self(commit.0)
    }
}

impl From<BranchName> for GitRef {
    fn from(branch: BranchName) -> Self {
        Self(branch.0)
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

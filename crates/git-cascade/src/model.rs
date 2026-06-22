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

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BranchName(String);

impl BranchName {
    pub fn new(value: &str) -> std::result::Result<Self, String> {
        value.parse()
    }

    pub(crate) fn from_git_unchecked(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<BranchName> for String {
    fn from(value: BranchName) -> Self {
        value.0
    }
}

impl std::fmt::Display for BranchName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

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

impl FromStr for BranchName {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        if value.is_empty() {
            return Err("branch name must not be empty".to_owned());
        }
        if value.starts_with("refs/") {
            return Err("branch name must not include refs/ prefix".to_owned());
        }
        if value.starts_with('-') {
            return Err("branch name must not start with '-'".to_owned());
        }
        if value.ends_with('/') || value.ends_with(".lock") {
            return Err("branch name has invalid suffix".to_owned());
        }
        if value.contains("..") || value.contains("@{") {
            return Err("branch name contains invalid sequence".to_owned());
        }
        if value
            .chars()
            .any(|ch| ch.is_control() || " ~^:?*[\\".contains(ch))
        {
            return Err("branch name contains invalid character".to_owned());
        }
        Ok(Self(value.to_owned()))
    }
}

impl FromStr for CommitId {
    type Err = Infallible;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self::new(value))
    }
}

impl FromStr for GitRef {
    type Err = Infallible;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self::new(value))
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
    /// Collapse each replayed branch into one commit before moving its children.
    Squash,
}

impl Strategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreserveForkPoints => "preserve-fork-points",
            Self::MoveToPlannedTips => "move-to-planned-tips",
            Self::MoveToCurrentTips => "move-to-current-tips",
            Self::Squash => "squash",
        }
    }
}

impl std::fmt::Display for Strategy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::BranchName;
    use std::str::FromStr;

    #[test]
    fn branch_name_rejects_refs_and_revision_syntax() {
        for invalid in [
            "",
            "refs/heads/topic",
            "origin/main^",
            "topic..main",
            "bad lock.lock",
        ] {
            assert!(
                BranchName::from_str(invalid).is_err(),
                "accepted `{invalid}`"
            );
        }
    }

    #[test]
    fn branch_name_accepts_path_components() {
        assert_eq!(
            BranchName::from_str("feature/topic").unwrap().as_str(),
            "feature/topic"
        );
    }
}

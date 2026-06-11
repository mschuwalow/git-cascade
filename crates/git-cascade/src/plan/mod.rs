mod generate;
mod topological;
mod validate;

use crate::encoding::{decode_component, encode_component};
use crate::{Error, Result};
pub use generate::{GenerateOptions, generate_plan, generate_stored_plan};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use time::OffsetDateTime;
pub use topological::branches_in_topological_order;
use uuid::Uuid;
pub use validate::{
    BranchRef, validate_branch_refs, validate_plan, validate_plan_for_apply,
    validate_unmapped_parents_for_apply,
};

/// Current plan schema version. Version 1 (linear commit lists) is still
/// accepted on read; see [`Plan::from_yaml`].
pub const PLAN_VERSION: u32 = 2;

/// The first commit on the tip's first-parent chain whose own first parent
/// lies outside `commits`. Its first parent is the branch's effective fork
/// point; apply substitutes it with the selected replay base so the chain is
/// transplanted even when it does not start at the node base.
pub fn first_parent_chain_root(commits: &[PlanCommit]) -> Option<&PlanCommit> {
    let by_oid = commits
        .iter()
        .map(|commit| (commit.oid.as_str(), commit))
        .collect::<std::collections::HashMap<_, _>>();
    let mut current = commits.last()?;
    loop {
        let parent = current.first_parent()?;
        match by_oid.get(parent) {
            Some(next) => current = next,
            None => return Some(current),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plan {
    pub version: u32,
    pub plan_id: PlanId,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    pub repository: Repository,
    pub source: Source,
    pub nodes: Vec<Node>,
    pub dependencies: Vec<Dependency>,
}

impl Plan {
    /// Deserializes a stored plan and normalizes it to the current schema.
    ///
    /// Version 1 plans recorded commits as plain oid strings of a linear
    /// range; parents are synthesized accordingly.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        let mut plan: Self = serde_yaml::from_str(yaml)?;
        plan.normalize_commit_parents();
        Ok(plan)
    }

    fn normalize_commit_parents(&mut self) {
        for node in &mut self.nodes {
            let mut previous = node.base.clone();
            for commit in &mut node.commits {
                if commit.parents.is_empty() {
                    commit.parents = vec![previous.clone()];
                }
                previous = commit.oid.clone();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlanId(Uuid);

impl PlanId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for PlanId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::str::FromStr for PlanId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

impl std::fmt::Display for PlanId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Serialize for PlanId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PlanId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PlanName(String);

impl PlanName {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if name.is_empty() {
            return Err(Error::InvalidPlanName {
                name,
                reason: "must not be empty".to_owned(),
            });
        }

        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn encoded(&self) -> String {
        encode_component(&self.0)
    }

    pub fn from_encoded(encoded: &str) -> Result<Self> {
        Self::new(decode_component(encoded)?)
    }
}

impl FromStr for PlanName {
    type Err = Error;

    fn from_str(name: &str) -> Result<Self> {
        Self::new(name)
    }
}

impl TryFrom<String> for PlanName {
    type Error = Error;

    fn try_from(name: String) -> Result<Self> {
        Self::new(name)
    }
}

impl From<PlanName> for String {
    fn from(name: PlanName) -> Self {
        name.0
    }
}

impl std::fmt::Display for PlanName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Repository {
    pub git_dir: String,
    pub head_at_generation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub base: String,
    pub tip: String,
}

/// A commit to replay, with its recorded parents.
///
/// Deserializes from either a plain oid string (version 1 plans; parents are
/// synthesized by [`Plan::from_yaml`]) or a structured `{oid, parents}` map.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlanCommit {
    pub oid: String,
    pub parents: Vec<String>,
}

impl PlanCommit {
    pub fn new(oid: impl Into<String>, parents: Vec<String>) -> Self {
        Self {
            oid: oid.into(),
            parents,
        }
    }

    pub fn is_merge(&self) -> bool {
        self.parents.len() > 1
    }

    pub fn first_parent(&self) -> Option<&str> {
        self.parents.first().map(String::as_str)
    }
}

impl<'de> Deserialize<'de> for PlanCommit {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Plain(String),
            Structured {
                oid: String,
                #[serde(default)]
                parents: Vec<String>,
            },
        }

        Ok(match Repr::deserialize(deserializer)? {
            Repr::Plain(oid) => Self {
                oid,
                parents: Vec::new(),
            },
            Repr::Structured { oid, parents } => Self { oid, parents },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Node {
    pub branch: String,
    pub tip: String,
    pub base: String,
    pub commits: Vec<PlanCommit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

impl Node {
    pub fn parent(&self) -> Option<&str> {
        self.parent.as_deref()
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn commits(&self) -> &[PlanCommit] {
        &self.commits
    }

    pub fn commit_oids(&self) -> impl Iterator<Item = &str> {
        self.commits.iter().map(|commit| commit.oid.as_str())
    }

    pub fn contains_commit(&self, oid: &str) -> bool {
        self.commit_oids().any(|commit| commit == oid)
    }

    pub fn is_root(&self) -> bool {
        self.parent.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dependency {
    pub parent: String,
    pub child: String,
}

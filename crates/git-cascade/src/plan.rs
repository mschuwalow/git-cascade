use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plan {
    pub version: u32,
    pub plan_id: PlanId,
    pub generated_at: String,
    pub repository: Repository,
    pub source: Source,
    pub nodes: Vec<Node>,
    pub dependencies: Vec<Dependency>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Repository {
    pub git_dir: String,
    pub head_at_generation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub old_base: String,
    pub old_tip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Node {
    pub branch: String,
    pub old_tip: String,
    #[serde(flatten)]
    pub kind: NodeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeKind {
    Root {
        old_base: String,
        commits: Vec<String>,
    },
    Dependent {
        parent: String,
        old_base: String,
        commits: Vec<String>,
    },
}

impl Node {
    pub fn parent(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Root { .. } => None,
            NodeKind::Dependent { parent, .. } => Some(parent),
        }
    }

    pub fn old_base(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Root { old_base, .. } => Some(old_base),
            NodeKind::Dependent { old_base, .. } => Some(old_base),
        }
    }

    pub fn commits(&self) -> &[String] {
        match &self.kind {
            NodeKind::Root { commits, .. } => commits,
            NodeKind::Dependent { commits, .. } => commits,
        }
    }

    pub fn is_root(&self) -> bool {
        matches!(self.kind, NodeKind::Root { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dependency {
    pub parent: String,
    pub child: String,
}

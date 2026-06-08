use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plan {
    pub version: u32,
    pub plan_id: String,
    pub generated_at: String,
    pub repository: Repository,
    pub source: Source,
    pub nodes: Vec<Node>,
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Repository {
    pub git_dir: String,
    pub head_at_generation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    pub anchor_branch: String,
    pub anchor_old_tip: String,
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
    Anchor,
    Dependent {
        parent: String,
        old_base: String,
        commits: Vec<String>,
    },
}

impl Node {
    pub fn parent(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Anchor => None,
            NodeKind::Dependent { parent, .. } => Some(parent),
        }
    }

    pub fn old_base(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Anchor => None,
            NodeKind::Dependent { old_base, .. } => Some(old_base),
        }
    }

    pub fn commits(&self) -> &[String] {
        match &self.kind {
            NodeKind::Anchor => &[],
            NodeKind::Dependent { commits, .. } => commits,
        }
    }

    pub fn is_anchor(&self) -> bool {
        matches!(self.kind, NodeKind::Anchor)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dependency {
    pub parent: String,
    pub child: String,
}

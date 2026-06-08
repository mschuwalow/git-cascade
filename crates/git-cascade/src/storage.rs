use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::encoding::{decode_component, encode_component};
use crate::git::Git;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlanKey(String);

impl PlanKey {
    pub fn from_anchor(anchor: impl Into<String>) -> Result<Self> {
        let anchor = anchor.into();
        if anchor.is_empty() {
            return Err(Error::InvalidPlanName {
                name: anchor,
                reason: "must not be empty".to_owned(),
            });
        }

        Ok(Self(anchor))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn encoded(&self) -> String {
        encode_component(&self.0)
    }

    pub fn from_encoded(encoded: &str) -> Result<Self> {
        Self::from_anchor(decode_component(encoded)?)
    }
}

impl FromStr for PlanKey {
    type Err = Error;

    fn from_str(name: &str) -> Result<Self> {
        Self::from_anchor(name)
    }
}

impl std::fmt::Display for PlanKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct Storage {
    common_dir: PathBuf,
}

impl Storage {
    pub fn discover(git: &Git) -> Result<Self> {
        Ok(Self {
            common_dir: git.git_common_dir()?,
        })
    }

    pub fn new(common_dir: impl Into<PathBuf>) -> Self {
        Self {
            common_dir: common_dir.into(),
        }
    }

    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    pub fn cascade_dir(&self) -> PathBuf {
        self.common_dir.join("cascade")
    }

    pub fn plans_dir(&self) -> PathBuf {
        self.cascade_dir().join("plans")
    }

    pub fn state_path(&self) -> PathBuf {
        self.cascade_dir().join("state.yaml")
    }

    pub fn worktrees_dir(&self) -> PathBuf {
        self.cascade_dir().join("worktrees")
    }

    pub fn plan_path(&self, key: &PlanKey) -> PathBuf {
        self.plans_dir().join(format!("{}.yaml", key.encoded()))
    }

    pub fn ensure_plans_dir(&self) -> Result<()> {
        let path = self.plans_dir();
        fs::create_dir_all(&path).map_err(|source| Error::IoWithPath { path, source })
    }

    pub fn ensure_cascade_dir(&self) -> Result<()> {
        let path = self.cascade_dir();
        fs::create_dir_all(&path).map_err(|source| Error::IoWithPath { path, source })
    }

    pub fn ensure_worktrees_dir(&self) -> Result<()> {
        let path = self.worktrees_dir();
        fs::create_dir_all(&path).map_err(|source| Error::IoWithPath { path, source })
    }

    pub fn read_plan(&self, key: &PlanKey) -> Result<String> {
        let path = self.plan_path(key);
        fs::read_to_string(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::PlanNotFound {
                    name: key.to_string(),
                    path,
                }
            } else {
                Error::IoWithPath { path, source }
            }
        })
    }

    pub fn delete_plan(&self, key: PlanKey) -> Result<()> {
        let path = self.plan_path(&key);
        fs::remove_file(&path).map_err(|source| Error::IoWithPath { path, source })
    }

    pub fn list_plan_keys(&self) -> Result<Vec<PlanKey>> {
        let path = self.plans_dir();
        if !path.exists() {
            return Ok(Vec::new());
        }

        let entries = fs::read_dir(&path).map_err(|source| Error::IoWithPath {
            path: path.clone(),
            source,
        })?;

        let mut names = Vec::new();
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_file() {
                continue;
            }

            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            let Some(encoded_name) = file_name.strip_suffix(".yaml") else {
                continue;
            };

            if let Ok(name) = PlanKey::from_encoded(encoded_name) {
                names.push(name);
            }
        }

        names.sort();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::{PlanKey, Storage};

    #[test]
    fn builds_repository_storage_paths() {
        let storage = Storage::new("/repo/.git");

        assert_eq!(storage.common_dir(), std::path::Path::new("/repo/.git"));
        assert_eq!(
            storage.plans_dir(),
            std::path::Path::new("/repo/.git/cascade/plans")
        );
        assert_eq!(
            storage.state_path(),
            std::path::Path::new("/repo/.git/cascade/state.yaml")
        );
        assert_eq!(
            storage.worktrees_dir(),
            std::path::Path::new("/repo/.git/cascade/worktrees")
        );
    }

    #[test]
    fn encodes_named_plan_paths() {
        let storage = Storage::new("/repo/.git");
        let name = PlanKey::from_anchor("feature/stack with spaces").unwrap();

        assert_eq!(
            storage.plan_path(&name),
            std::path::Path::new(
                "/repo/.git/cascade/plans/ZmVhdHVyZS9zdGFjayB3aXRoIHNwYWNlcw.yaml"
            )
        );
    }

    #[test]
    fn decodes_plan_names_from_storage_components() {
        let encoded = PlanKey::from_anchor("feature/stack with spaces")
            .unwrap()
            .encoded();

        assert_eq!(
            PlanKey::from_encoded(&encoded).unwrap().as_str(),
            "feature/stack with spaces"
        );
    }
}

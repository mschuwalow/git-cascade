use std::fs;
use std::path::{Path, PathBuf};

use crate::git::Git;
use crate::{Error, Result, plan_name};

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

    pub fn named_plan_path(&self, name: &str) -> Result<PathBuf> {
        plan_name::validate(name)?;
        Ok(self.plans_dir().join(format!("{name}.yaml")))
    }

    pub fn ensure_plans_dir(&self) -> Result<()> {
        let path = self.plans_dir();
        fs::create_dir_all(&path).map_err(|source| Error::IoWithPath { path, source })
    }

    pub fn read_named_plan(&self, name: &str) -> Result<String> {
        let path = self.named_plan_path(name)?;
        fs::read_to_string(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::PlanNotFound {
                    name: name.to_owned(),
                    path,
                }
            } else {
                Error::IoWithPath { path, source }
            }
        })
    }

    pub fn list_plan_names(&self) -> Result<Vec<String>> {
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
            let Some(name) = file_name.strip_suffix(".yaml") else {
                continue;
            };

            if plan_name::validate(name).is_ok() {
                names.push(name.to_owned());
            }
        }

        names.sort();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::Storage;

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
    fn rejects_invalid_named_plan_paths() {
        let storage = Storage::new("/repo/.git");

        assert!(storage.named_plan_path("../stack").is_err());
    }
}

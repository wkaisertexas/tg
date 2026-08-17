use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Repository {
    pub invocation_root: PathBuf,
    pub search_root: PathBuf,
    pub git_aware: bool,
}

impl Repository {
    pub fn discover(root: &Path) -> Result<Self> {
        if !root.is_dir() {
            bail!(
                "root folder does not exist or is not a directory: {}",
                root.display()
            );
        }
        let invocation_root = root
            .canonicalize()
            .with_context(|| format!("cannot read root folder {}", root.display()))?;
        let mut cursor = invocation_root.as_path();
        let search_root = loop {
            if cursor.join(".git").exists() {
                break Some(cursor.to_path_buf());
            }
            match cursor.parent() {
                Some(parent) => cursor = parent,
                None => break None,
            }
        };
        Ok(match search_root {
            Some(search_root) => Self {
                invocation_root,
                search_root,
                git_aware: true,
            },
            None => Self {
                search_root: invocation_root.clone(),
                invocation_root,
                git_aware: false,
            },
        })
    }

    pub fn relative(&self, path: &Path) -> Result<String> {
        let canonical = path.canonicalize()?;
        let relative = canonical
            .strip_prefix(&self.search_root)
            .context("path escapes the search root")?;
        Ok(relative.to_string_lossy().replace('\\', "/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_this_repository_from_nested_directory() {
        let repo = Repository::discover(Path::new("docs")).unwrap();
        assert!(repo.git_aware);
        assert_eq!(
            repo.relative(&repo.search_root.join("docs/spec.md"))
                .unwrap(),
            "docs/spec.md"
        );
    }

    #[test]
    fn discovery_uses_the_nearest_git_ancestor() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        let nested = temp.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();

        let repo = Repository::discover(&nested).unwrap();
        assert!(repo.git_aware);
        assert_eq!(repo.invocation_root, nested.canonicalize().unwrap());
        assert_eq!(repo.search_root, temp.path().canonicalize().unwrap());
    }

    #[test]
    fn discovery_falls_back_to_the_requested_directory_without_git() {
        let temp = tempfile::tempdir().unwrap();
        let repo = Repository::discover(temp.path()).unwrap();
        assert!(!repo.git_aware);
        assert_eq!(repo.invocation_root, temp.path().canonicalize().unwrap());
        assert_eq!(repo.search_root, repo.invocation_root);
    }

    #[test]
    fn discovery_rejects_files_and_missing_directories() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("prompt.md");
        fs::write(&file, "prompt").unwrap();
        assert!(Repository::discover(&file).is_err());
        assert!(Repository::discover(&temp.path().join("missing")).is_err());
    }
}

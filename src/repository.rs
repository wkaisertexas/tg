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
        let search_root = find_git_ancestor(&invocation_root);
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

    /// Resolves editor repository context in external-editor-safe precedence.
    ///
    /// Explicit CLI and environment roots retain `discover` semantics. Without
    /// either override, a Git worktree containing the process CWD wins over a
    /// worktree containing the edited file, since external editors commonly
    /// receive temporary files outside the project.
    pub fn for_editor(
        explicit_root: Option<&Path>,
        environment_root: Option<&Path>,
        cwd: &Path,
        file: Option<&Path>,
    ) -> Result<Self> {
        if let Some(root) = explicit_root.or(environment_root) {
            return Self::discover(root);
        }

        if !cwd.is_dir() {
            bail!(
                "current working directory does not exist or is not a directory: {}",
                cwd.display()
            );
        }
        let canonical_cwd = cwd
            .canonicalize()
            .with_context(|| format!("cannot read current working directory {}", cwd.display()))?;
        if let Some(search_root) = find_git_ancestor(&canonical_cwd) {
            return Ok(Self {
                invocation_root: canonical_cwd,
                search_root,
                git_aware: true,
            });
        }

        if let Some(file) = file {
            let absolute_file = if file.is_absolute() {
                file.to_path_buf()
            } else {
                canonical_cwd.join(file)
            };
            let parent = absolute_file
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(&canonical_cwd);
            let canonical_parent = parent
                .canonicalize()
                .with_context(|| format!("cannot read edited file parent {}", parent.display()))?;
            if let Some(search_root) = find_git_ancestor(&canonical_parent) {
                return Ok(Self {
                    invocation_root: canonical_parent,
                    search_root,
                    git_aware: true,
                });
            }
        }

        Ok(Self {
            invocation_root: canonical_cwd.clone(),
            search_root: canonical_cwd,
            git_aware: false,
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

fn find_git_ancestor(root: &Path) -> Option<PathBuf> {
    root.ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .map(Path::to_path_buf)
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

    #[test]
    fn editor_context_prefers_overrides_then_cwd_git_then_file_git() {
        let temp = tempfile::tempdir().unwrap();
        let explicit = temp.path().join("explicit");
        let environment = temp.path().join("environment");
        let cwd_repo = temp.path().join("cwd-repo");
        let file_repo = temp.path().join("file-repo");
        for root in [&explicit, &environment, &cwd_repo, &file_repo] {
            fs::create_dir_all(root.join("nested")).unwrap();
            fs::create_dir(root.join(".git")).unwrap();
        }
        let cwd = cwd_repo.join("nested");
        let file = file_repo.join("nested/prompt.md");

        let repository =
            Repository::for_editor(Some(&explicit), Some(&environment), &cwd, Some(&file)).unwrap();
        assert_eq!(repository.search_root, explicit.canonicalize().unwrap());

        let repository =
            Repository::for_editor(None, Some(&environment), &cwd, Some(&file)).unwrap();
        assert_eq!(repository.search_root, environment.canonicalize().unwrap());

        let repository = Repository::for_editor(None, None, &cwd, Some(&file)).unwrap();
        assert_eq!(repository.search_root, cwd_repo.canonicalize().unwrap());

        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let repository = Repository::for_editor(None, None, &outside, Some(&file)).unwrap();
        assert_eq!(repository.search_root, file_repo.canonicalize().unwrap());
    }

    #[test]
    fn editor_context_falls_back_to_cwd_without_git() {
        let temp = tempfile::tempdir().unwrap();
        let file_parent = temp.path().join("files");
        fs::create_dir(&file_parent).unwrap();
        let repository =
            Repository::for_editor(None, None, temp.path(), Some(&file_parent.join("new.md")))
                .unwrap();
        assert!(!repository.git_aware);
        assert_eq!(repository.search_root, temp.path().canonicalize().unwrap());
    }
}

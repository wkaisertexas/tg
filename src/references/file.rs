use super::model::{
    CandidateDisplay, CandidateId, CandidateTokenSource, ContextCost, FileOrigin, FileTarget,
    FileVersion, Preview, PreviewLine, QueryRequest, QueryScope, ReferenceCandidate, ReferenceKind,
    ReferenceTarget, ValidatedTarget,
};
use super::{CancellationFlag, ReferenceProvider};
use crate::search::{self, SearchMode};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct FileProvider {
    root: PathBuf,
    canonical_root: PathBuf,
    kind: ReferenceKind,
    origin: FileOrigin,
    leader: String,
    files: Vec<PathBuf>,
}

impl FileProvider {
    pub fn new(root: &Path, kind: ReferenceKind, leader: impl Into<String>) -> Result<Self> {
        Self::with_broad_excludes(root, kind, leader, &[])
    }

    pub fn with_broad_excludes(
        root: &Path,
        kind: ReferenceKind,
        leader: impl Into<String>,
        broad_excludes: &[String],
    ) -> Result<Self> {
        let (mode, origin) = match kind {
            ReferenceKind::GitFile => (SearchMode::GitAware, FileOrigin::GitAware),
            ReferenceKind::BroadFile => (SearchMode::Broad, FileOrigin::Broad),
            _ => bail!("file provider requires a file reference kind"),
        };
        let canonical_root = root
            .canonicalize()
            .with_context(|| format!("cannot read search root {}", root.display()))?;
        let files = search::walk_with_excludes(root, mode, broad_excludes);
        Ok(Self {
            root: root.to_path_buf(),
            canonical_root,
            kind,
            origin,
            leader: leader.into(),
            files,
        })
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    fn target(&self, relative: &str) -> Result<FileTarget> {
        let canonical_path = search::resolve_exact(&self.root, relative)?;
        anyhow::ensure!(
            canonical_path.starts_with(&self.canonical_root),
            "file is outside search root"
        );
        Ok(FileTarget {
            source_version: Some(file_version(&canonical_path)?),
            relative_path: canonical_path
                .strip_prefix(&self.canonical_root)
                .context("file is outside search root")?
                .to_string_lossy()
                .replace('\\', "/"),
            canonical_path,
            origin: self.origin,
        })
    }
}

impl ReferenceProvider for FileProvider {
    fn kind(&self) -> ReferenceKind {
        self.kind
    }

    fn query(
        &self,
        request: QueryRequest,
        cancellation: &CancellationFlag,
    ) -> Result<Vec<ReferenceCandidate>> {
        anyhow::ensure!(
            request.scope == QueryScope::Repository,
            "file provider only supports repository queries"
        );
        if cancellation.is_cancelled() {
            return Ok(Vec::new());
        }
        Ok(
            search::find(&self.root, &self.files, &request.query, request.limit)
                .into_iter()
                .take_while(|_| !cancellation.is_cancelled())
                .map(|found| ReferenceCandidate {
                    id: CandidateId {
                        provider: self.kind,
                        opaque: found.relative.clone(),
                    },
                    generation: request.generation,
                    kind: self.kind,
                    friendly_text: format!("{}{}", self.leader, found.relative),
                    display: CandidateDisplay {
                        primary: found.relative,
                        match_indices: found.indices,
                        ..CandidateDisplay::default()
                    },
                    context_cost: ContextCost::Pending,
                    file_context_cost: None,
                    source_version: None,
                    token_source: Some(CandidateTokenSource::File { path: found.path }),
                })
                .collect(),
        )
    }

    fn resolve(&self, id: &CandidateId) -> Result<ReferenceTarget> {
        anyhow::ensure!(
            id.provider == self.kind,
            "candidate belongs to another provider"
        );
        Ok(ReferenceTarget::File(self.target(&id.opaque)?))
    }

    fn validate(&self, target: &ReferenceTarget) -> Result<ValidatedTarget> {
        let ReferenceTarget::File(file) = target else {
            bail!("file provider cannot validate this target")
        };
        anyhow::ensure!(
            file.origin == self.origin,
            "file target has the wrong origin"
        );
        Ok(ValidatedTarget {
            target: ReferenceTarget::File(self.target(&file.relative_path)?),
            context_cost: ContextCost::Pending,
        })
    }

    fn lower(&self, target: &ValidatedTarget) -> Result<String> {
        let ReferenceTarget::File(file) = &target.target else {
            bail!("file provider cannot lower this target")
        };
        Ok(file.relative_path.clone())
    }

    fn preview(&self, target: &ReferenceTarget) -> Result<Option<Preview>> {
        let ReferenceTarget::File(file) = target else {
            bail!("file provider cannot preview this target")
        };
        let Ok(source) = std::fs::read_to_string(&file.canonical_path) else {
            return Ok(None);
        };
        Ok(Some(Preview {
            title: Some(file.relative_path.clone()),
            lines: source
                .lines()
                .enumerate()
                .map(|(index, text)| PreviewLine {
                    number: Some(index + 1),
                    text: text.to_owned(),
                })
                .collect(),
            highlighted_lines: None,
        }))
    }

    fn context_cost(&self, _target: &ReferenceTarget) -> Result<ContextCost> {
        Ok(ContextCost::Pending)
    }
}

pub fn file_version(path: &Path) -> Result<FileVersion> {
    Ok(read_versioned(path)?.version)
}

pub(crate) struct VersionedFile {
    pub bytes: Vec<u8>,
    pub version: FileVersion,
}

pub(crate) fn read_versioned(path: &Path) -> Result<VersionedFile> {
    for _ in 0..2 {
        let mut file = File::open(path)?;
        let before = file.metadata()?;
        let mut bytes = Vec::with_capacity(before.len() as usize);
        file.read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        if before.len() == after.len() && before.modified().ok() == after.modified().ok() {
            return Ok(VersionedFile {
                version: FileVersion {
                    size: after.len(),
                    modified: after.modified().ok(),
                    content_sha256: Sha256::digest(&bytes).into(),
                },
                bytes,
            });
        }
    }
    bail!("file changed while it was being read")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::model::GenerationId;
    use std::collections::BTreeSet;
    use std::fs;

    fn names(paths: &[PathBuf], root: &Path) -> BTreeSet<String> {
        paths
            .iter()
            .map(|path| {
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    #[test]
    fn providers_preserve_git_and_broad_walk_behavior() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join(".gitignore"), "ignored/\n").unwrap();
        fs::create_dir(temp.path().join("ignored")).unwrap();
        fs::write(temp.path().join("ignored/generated.txt"), "generated").unwrap();
        fs::write(temp.path().join("visible.txt"), "visible").unwrap();
        fs::write(temp.path().join(".git/config"), "secret").unwrap();

        let git = FileProvider::new(temp.path(), ReferenceKind::GitFile, "@").unwrap();
        let broad = FileProvider::new(temp.path(), ReferenceKind::BroadFile, "%").unwrap();

        assert_eq!(
            names(git.files(), temp.path()),
            names(
                &search::walk(temp.path(), SearchMode::GitAware),
                temp.path()
            )
        );
        assert_eq!(
            names(broad.files(), temp.path()),
            names(&search::walk(temp.path(), SearchMode::Broad), temp.path())
        );
        assert!(!names(git.files(), temp.path()).contains("ignored/generated.txt"));
        assert!(names(broad.files(), temp.path()).contains("ignored/generated.txt"));
        assert!(!names(broad.files(), temp.path()).contains(".git/config"));

        let results = broad
            .query(
                QueryRequest {
                    generation: GenerationId(7),
                    query: "generated".into(),
                    scope: QueryScope::Repository,
                    limit: 100,
                    typed_leader: "%".into(),
                },
                &CancellationFlag::default(),
            )
            .unwrap();
        assert_eq!(results[0].friendly_text, "%ignored/generated.txt");
        assert_eq!(results[0].generation, GenerationId(7));
    }

    #[cfg(unix)]
    #[test]
    fn resolving_a_symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            root.path().join("escape.txt"),
        )
        .unwrap();

        let provider = FileProvider::new(root.path(), ReferenceKind::BroadFile, "%").unwrap();
        let error = provider
            .resolve(&CandidateId {
                provider: ReferenceKind::BroadFile,
                opaque: "escape.txt".into(),
            })
            .unwrap_err();
        assert!(error.to_string().contains("outside search root"));
    }

    #[test]
    fn versions_include_content_not_only_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("same-size.txt");
        fs::write(&path, "first").unwrap();
        let first = file_version(&path).unwrap();
        fs::write(&path, "other").unwrap();
        let other = file_version(&path).unwrap();
        assert_eq!(first.size, other.size);
        assert_ne!(first.content_sha256, other.content_sha256);
    }
}

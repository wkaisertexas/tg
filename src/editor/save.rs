//! Conflict-safe, atomic writes for the editor's single file.
//!
//! Callers supply the already-rendered UTF-8 bytes. This module deliberately
//! does not know about friendly text or reference lowering.

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, Metadata, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    identity: FileIdentity,
    length: u64,
    modified: Option<SystemTime>,
    mode: u32,
    sha256: [u8; 32],
}

#[derive(Debug, Clone)]
enum BaselineState {
    Existing {
        fingerprint: FileFingerprint,
        permissions: Permissions,
    },
    Missing,
}

/// An opened destination and the disk state against which its next save is checked.
#[derive(Debug, Clone)]
pub struct SaveTarget {
    logical_path: PathBuf,
    target_path: PathBuf,
    canonical_parent: PathBuf,
    baseline: BaselineState,
}

/// Bytes read from an existing file, or an empty buffer for a new file.
#[derive(Debug)]
pub struct OpenedTarget {
    pub bytes: Vec<u8>,
    pub target: SaveTarget,
}

impl SaveTarget {
    /// Opens an existing regular file or prepares a baseline for a new file.
    ///
    /// The returned bytes and baseline come from the same stable read. UTF-8
    /// validation remains the document layer's responsibility.
    pub fn open(path: &Path) -> Result<OpenedTarget, SaveError> {
        let logical_path = absolute_path(path)?;
        let parent = logical_path.parent().ok_or_else(|| {
            SaveError::invalid(&logical_path, "the destination has no parent directory")
        })?;
        let canonical_parent = fs::canonicalize(parent)
            .map_err(|error| SaveError::io("canonicalize parent", parent, error))?;
        if !fs::metadata(&canonical_parent)
            .map_err(|error| SaveError::io("inspect parent", &canonical_parent, error))?
            .is_dir()
        {
            return Err(SaveError::invalid(
                &logical_path,
                "the destination parent is not a directory",
            ));
        }
        let file_name = logical_path
            .file_name()
            .ok_or_else(|| SaveError::invalid(&logical_path, "the destination has no file name"))?;

        match fs::symlink_metadata(&logical_path) {
            Ok(_) => {
                let target_path = fs::canonicalize(&logical_path).map_err(|error| {
                    SaveError::invalid(
                        &logical_path,
                        format!("cannot resolve the existing destination: {error}"),
                    )
                })?;
                let (bytes, fingerprint, permissions) = stable_read(&target_path)?;
                if !fs::metadata(&target_path)
                    .map_err(|error| SaveError::io("inspect destination", &target_path, error))?
                    .is_file()
                {
                    return Err(SaveError::invalid(
                        &logical_path,
                        "the destination is not a regular file",
                    ));
                }
                let canonical_parent = target_path
                    .parent()
                    .expect("a canonical file has a parent")
                    .to_path_buf();
                Ok(OpenedTarget {
                    bytes,
                    target: Self {
                        logical_path,
                        target_path,
                        canonical_parent,
                        baseline: BaselineState::Existing {
                            fingerprint,
                            permissions,
                        },
                    },
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let target_path = canonical_parent.join(file_name);
                ensure_missing(
                    &target_path,
                    &logical_path,
                    "the destination appeared while it was being opened",
                )?;
                Ok(OpenedTarget {
                    bytes: Vec::new(),
                    target: Self {
                        logical_path,
                        target_path,
                        canonical_parent,
                        baseline: BaselineState::Missing,
                    },
                })
            }
            Err(error) => Err(SaveError::io("inspect destination", &logical_path, error)),
        }
    }

    pub fn logical_path(&self) -> &Path {
        &self.logical_path
    }

    pub fn existed_at_baseline(&self) -> bool {
        matches!(self.baseline, BaselineState::Existing { .. })
    }

    fn verify_unchanged(&self) -> Result<(), SaveError> {
        match &self.baseline {
            BaselineState::Existing { fingerprint, .. } => {
                let resolved = fs::canonicalize(&self.logical_path).map_err(|error| {
                    SaveError::conflict(
                        &self.logical_path,
                        format!("the opened destination can no longer be resolved: {error}"),
                    )
                })?;
                if resolved != self.target_path {
                    return Err(SaveError::conflict(
                        &self.logical_path,
                        "the destination path now resolves to a different file",
                    ));
                }
                let current = fingerprint_file(&self.target_path)?;
                if &current != fingerprint {
                    return Err(SaveError::conflict(
                        &self.logical_path,
                        "the destination changed outside the editor",
                    ));
                }
            }
            BaselineState::Missing => {
                let logical_parent = self.logical_path.parent().expect("opened path has parent");
                let current_parent = fs::canonicalize(logical_parent).map_err(|error| {
                    SaveError::conflict(
                        &self.logical_path,
                        format!("the destination parent can no longer be resolved: {error}"),
                    )
                })?;
                if current_parent != self.canonical_parent {
                    return Err(SaveError::conflict(
                        &self.logical_path,
                        "the destination parent now resolves to a different directory",
                    ));
                }
                ensure_missing(
                    &self.logical_path,
                    &self.logical_path,
                    "the destination was created outside the editor",
                )?;
                if self.target_path != self.logical_path {
                    ensure_missing(
                        &self.target_path,
                        &self.logical_path,
                        "the destination was created outside the editor",
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// A point at which tests may inject a pre-rename failure or external mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailurePoint {
    AfterCreate,
    AfterPermissions,
    AfterWrite,
    AfterFlush,
    AfterSync,
    BeforeFinalCheck,
    BeforeRename,
}

pub trait SaveHooks {
    fn check(&self, point: FailurePoint) -> io::Result<()>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoHooks;

impl SaveHooks for NoHooks {
    fn check(&self, _point: FailurePoint) -> io::Result<()> {
        Ok(())
    }
}

/// Performs same-directory atomic writes, with injectable pre-rename hooks.
pub struct AtomicSaver<H = NoHooks> {
    hooks: H,
}

impl Default for AtomicSaver<NoHooks> {
    fn default() -> Self {
        Self { hooks: NoHooks }
    }
}

impl AtomicSaver<NoHooks> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<H: SaveHooks> AtomicSaver<H> {
    pub fn with_hooks(hooks: H) -> Self {
        Self { hooks }
    }

    /// Writes `bytes` and returns the refreshed post-save baseline.
    pub fn save(&self, target: &SaveTarget, bytes: &[u8]) -> Result<SaveTarget, SaveError> {
        target.verify_unchanged()?;
        let (mut temp, temp_path) = create_temp(&target.canonical_parent)?;
        let mut cleanup = TempCleanup::new(temp_path.clone());
        self.hook(FailurePoint::AfterCreate, &temp_path)?;

        if let BaselineState::Existing { permissions, .. } = &target.baseline {
            fs::set_permissions(&temp_path, permissions.clone())
                .map_err(|error| SaveError::io("preserve permissions", &temp_path, error))?;
        }
        self.hook(FailurePoint::AfterPermissions, &temp_path)?;

        temp.write_all(bytes)
            .map_err(|error| SaveError::io("write temporary file", &temp_path, error))?;
        self.hook(FailurePoint::AfterWrite, &temp_path)?;
        temp.flush()
            .map_err(|error| SaveError::io("flush temporary file", &temp_path, error))?;
        self.hook(FailurePoint::AfterFlush, &temp_path)?;
        temp.sync_all()
            .map_err(|error| SaveError::io("sync temporary file", &temp_path, error))?;
        self.hook(FailurePoint::AfterSync, &temp_path)?;
        let committed_metadata = temp
            .metadata()
            .map_err(|error| SaveError::io("inspect temporary file", &temp_path, error))?;
        let committed_permissions = committed_metadata.permissions();
        let committed_fingerprint =
            metadata_fingerprint(&committed_metadata, Some(hash_bytes(bytes)));
        drop(temp);

        self.hook(FailurePoint::BeforeFinalCheck, &temp_path)?;
        target.verify_unchanged()?;
        self.hook(FailurePoint::BeforeRename, &temp_path)?;
        fs::rename(&temp_path, &target.target_path)
            .map_err(|error| SaveError::io("replace destination", &target.target_path, error))?;
        cleanup.disarm();

        Ok(SaveTarget {
            logical_path: target.logical_path.clone(),
            target_path: target.target_path.clone(),
            canonical_parent: target.canonical_parent.clone(),
            baseline: BaselineState::Existing {
                fingerprint: committed_fingerprint,
                permissions: committed_permissions,
            },
        })
    }

    fn hook(&self, point: FailurePoint, path: &Path) -> Result<(), SaveError> {
        self.hooks
            .check(point)
            .map_err(|error| SaveError::io("injected save hook", path, error))
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, SaveError> {
    if path.as_os_str().is_empty() {
        return Err(SaveError::invalid(path, "the destination path is empty"));
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| SaveError::io("resolve current directory", path, error))
    }
}

fn ensure_missing(
    path: &Path,
    logical_path: &Path,
    message: &'static str,
) -> Result<(), SaveError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(SaveError::conflict(logical_path, message)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SaveError::io("inspect destination", path, error)),
    }
}

fn stable_read(path: &Path) -> Result<(Vec<u8>, FileFingerprint, Permissions), SaveError> {
    let before = fs::metadata(path).map_err(|error| SaveError::io("inspect file", path, error))?;
    if !before.is_file() {
        return Err(SaveError::invalid(
            path,
            "the destination is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|error| SaveError::io("read file", path, error))?;
    let after = fs::metadata(path).map_err(|error| SaveError::io("inspect file", path, error))?;
    let before_metadata = metadata_fingerprint(&before, None);
    let after_metadata = metadata_fingerprint(&after, None);
    if before_metadata != after_metadata || bytes.len() as u64 != after.len() {
        return Err(SaveError::conflict(
            path,
            "the file changed while it was read",
        ));
    }
    let permissions = after.permissions();
    let fingerprint = metadata_fingerprint(&after, Some(hash_bytes(&bytes)));
    Ok((bytes, fingerprint, permissions))
}

fn fingerprint_file(path: &Path) -> Result<FileFingerprint, SaveError> {
    stable_read(path).map(|(_, fingerprint, _)| fingerprint)
}

fn metadata_fingerprint(metadata: &Metadata, sha256: Option<[u8; 32]>) -> FileFingerprint {
    FileFingerprint {
        identity: FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        length: metadata.len(),
        modified: metadata.modified().ok(),
        mode: metadata.mode(),
        sha256: sha256.unwrap_or([0; 32]),
    }
}

fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn create_temp(parent: &Path) -> Result<(File, PathBuf), SaveError> {
    for _ in 0..128 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".tg-save-{}-{sequence}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(SaveError::io("create temporary file", &path, error)),
        }
    }
    Err(SaveError::invalid(
        parent,
        "could not allocate a unique temporary file",
    ))
}

struct TempCleanup {
    path: PathBuf,
    armed: bool,
}

impl TempCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug)]
pub enum SaveError {
    InvalidTarget {
        path: PathBuf,
        message: String,
    },
    Conflict {
        path: PathBuf,
        message: String,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

impl SaveError {
    fn invalid(path: &Path, message: impl Into<String>) -> Self {
        Self::InvalidTarget {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }

    fn conflict(path: &Path, message: impl Into<String>) -> Self {
        Self::Conflict {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }

    fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }

    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict { .. })
    }
}

impl fmt::Display for SaveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget { path, message } => {
                write!(formatter, "{}: {message}", path.display())
            }
            Self::Conflict { path, message } => {
                write!(formatter, "{}: write conflict: {message}", path.display())
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::Arc;

    struct Hook(Arc<dyn Fn(FailurePoint) -> io::Result<()> + Send + Sync>);

    impl SaveHooks for Hook {
        fn check(&self, point: FailurePoint) -> io::Result<()> {
            (self.0)(point)
        }
    }

    fn temp_files(directory: &Path) -> Vec<PathBuf> {
        fs::read_dir(directory)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".tg-save-"))
            })
            .collect()
    }

    #[test]
    fn existing_and_new_files_save_atomically_and_refresh_the_baseline() {
        let directory = tempfile::tempdir().unwrap();
        let existing = directory.path().join("existing.txt");
        fs::write(&existing, b"before").unwrap();
        fs::set_permissions(&existing, Permissions::from_mode(0o640)).unwrap();
        let opened = SaveTarget::open(&existing).unwrap();
        assert_eq!(opened.bytes, b"before");
        assert!(opened.target.existed_at_baseline());

        let refreshed = AtomicSaver::new()
            .save(&opened.target, "after λ".as_bytes())
            .unwrap();
        assert_eq!(fs::read(&existing).unwrap(), "after λ".as_bytes());
        assert_eq!(
            fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
            0o640
        );
        AtomicSaver::new().save(&refreshed, b"again").unwrap();
        assert_eq!(fs::read(&existing).unwrap(), b"again");

        let new_path = directory.path().join("new.txt");
        let new_target = SaveTarget::open(&new_path).unwrap();
        assert!(new_target.bytes.is_empty());
        assert!(!new_target.target.existed_at_baseline());
        AtomicSaver::new()
            .save(&new_target.target, b"created")
            .unwrap();
        assert_eq!(fs::read(&new_path).unwrap(), b"created");
        assert!(temp_files(directory.path()).is_empty());
    }

    #[test]
    fn external_content_inode_and_new_file_changes_are_conflicts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("prompt.txt");
        fs::write(&path, b"original").unwrap();
        let content_target = SaveTarget::open(&path).unwrap().target;
        fs::write(&path, b"external").unwrap();
        let error = AtomicSaver::new()
            .save(&content_target, b"editor")
            .unwrap_err();
        assert!(error.is_conflict());
        assert_eq!(fs::read(&path).unwrap(), b"external");

        fs::write(&path, b"original").unwrap();
        let inode_target = SaveTarget::open(&path).unwrap().target;
        let replacement = directory.path().join("replacement");
        fs::write(&replacement, b"replacement").unwrap();
        fs::rename(&replacement, &path).unwrap();
        let error = AtomicSaver::new()
            .save(&inode_target, b"editor")
            .unwrap_err();
        assert!(error.is_conflict());
        assert_eq!(fs::read(&path).unwrap(), b"replacement");

        let new_path = directory.path().join("new.txt");
        let new_target = SaveTarget::open(&new_path).unwrap().target;
        fs::write(&new_path, b"appeared").unwrap();
        let error = AtomicSaver::new().save(&new_target, b"editor").unwrap_err();
        assert!(error.is_conflict());
        assert_eq!(fs::read(&new_path).unwrap(), b"appeared");
    }

    #[test]
    fn symlink_retarget_and_new_parent_retarget_are_conflicts() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        let link = directory.path().join("prompt.txt");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        symlink(&first, &link).unwrap();
        let target = SaveTarget::open(&link).unwrap().target;
        fs::remove_file(&link).unwrap();
        symlink(&second, &link).unwrap();
        let error = AtomicSaver::new().save(&target, b"editor").unwrap_err();
        assert!(error.is_conflict());
        assert_eq!(fs::read(&first).unwrap(), b"first");
        assert_eq!(fs::read(&second).unwrap(), b"second");

        let parent_one = directory.path().join("one");
        let parent_two = directory.path().join("two");
        fs::create_dir(&parent_one).unwrap();
        fs::create_dir(&parent_two).unwrap();
        let parent_link = directory.path().join("current");
        symlink(&parent_one, &parent_link).unwrap();
        let new_target = SaveTarget::open(&parent_link.join("new.txt"))
            .unwrap()
            .target;
        fs::remove_file(&parent_link).unwrap();
        symlink(&parent_two, &parent_link).unwrap();
        let error = AtomicSaver::new().save(&new_target, b"editor").unwrap_err();
        assert!(error.is_conflict());
        assert!(!parent_one.join("new.txt").exists());
        assert!(!parent_two.join("new.txt").exists());
    }

    #[test]
    fn every_injected_pre_rename_failure_preserves_destination_and_cleans_temp() {
        let points = [
            FailurePoint::AfterCreate,
            FailurePoint::AfterPermissions,
            FailurePoint::AfterWrite,
            FailurePoint::AfterFlush,
            FailurePoint::AfterSync,
            FailurePoint::BeforeFinalCheck,
            FailurePoint::BeforeRename,
        ];
        for point in points {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("prompt.txt");
            fs::write(&path, b"original").unwrap();
            let target = SaveTarget::open(&path).unwrap().target;
            let saver = AtomicSaver::with_hooks(Hook(Arc::new(move |seen| {
                if seen == point {
                    Err(io::Error::other("injected"))
                } else {
                    Ok(())
                }
            })));
            assert!(saver.save(&target, b"replacement").is_err(), "{point:?}");
            assert_eq!(fs::read(&path).unwrap(), b"original", "{point:?}");
            assert!(temp_files(directory.path()).is_empty(), "{point:?}");
        }
    }

    #[test]
    fn final_check_catches_a_change_after_the_temp_is_synced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("prompt.txt");
        fs::write(&path, b"original").unwrap();
        let target = SaveTarget::open(&path).unwrap().target;
        let changed_path = path.clone();
        let saver = AtomicSaver::with_hooks(Hook(Arc::new(move |point| {
            if point == FailurePoint::BeforeFinalCheck {
                fs::write(&changed_path, b"external")?;
            }
            Ok(())
        })));

        let error = saver.save(&target, b"editor").unwrap_err();
        assert!(error.is_conflict());
        assert_eq!(fs::read(&path).unwrap(), b"external");
        assert!(temp_files(directory.path()).is_empty());
    }
}

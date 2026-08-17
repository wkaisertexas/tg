use crate::references::model::{AcceptedReference, ResolvedReference, TextRange};
use anyhow::{Context, Result, bail, ensure};
use std::fs::Permissions;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub enum DocumentPath {
    Unnamed,
    New {
        logical: PathBuf,
    },
    Existing {
        logical: PathBuf,
        canonical: PathBuf,
        original_len: u64,
        original_modified: Option<SystemTime>,
        original_permissions: Permissions,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub range: TextRange,
    pub replacement: String,
}

impl TextEdit {
    pub fn new(range: TextRange, replacement: impl Into<String>) -> Self {
        Self {
            range,
            replacement: replacement.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    text: String,
    references: Vec<ResolvedReference>,
}

#[derive(Debug, Clone)]
pub struct DocumentSnapshot {
    text: String,
    references: Vec<ResolvedReference>,
    revision: u64,
}

impl DocumentSnapshot {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn references(&self) -> &[ResolvedReference] {
        &self.references
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Produces lowered text without mutating the friendly snapshot.
    pub fn lower(
        &self,
        mut replacement: impl FnMut(&ResolvedReference) -> Result<String>,
    ) -> Result<LoweredSnapshot> {
        let mut text = self.text.clone();
        for reference in self.references.iter().rev() {
            let bytes = byte_range(&text, reference.range)?;
            text.replace_range(bytes, &replacement(reference)?);
        }
        Ok(LoweredSnapshot {
            revision: self.revision,
            text,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredSnapshot {
    pub revision: u64,
    pub text: String,
}

#[derive(Debug)]
pub struct Document {
    path: DocumentPath,
    state: State,
    saved_state: State,
    revision: u64,
    saved_revision: u64,
    undo: Vec<State>,
    redo: Vec<State>,
    insert_start: Option<State>,
}

impl Document {
    pub fn unnamed() -> Self {
        Self::from_state(DocumentPath::Unnamed, String::new())
    }

    pub fn from_text(text: impl Into<String>) -> Self {
        Self::from_state(DocumentPath::Unnamed, text.into())
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let logical = path.as_ref().to_path_buf();
        match std::fs::metadata(&logical) {
            Ok(metadata) => {
                ensure!(metadata.is_file(), "document path is not a regular file");
                let bytes = std::fs::read(&logical)
                    .with_context(|| format!("cannot read document {}", logical.display()))?;
                let text = String::from_utf8(bytes).context("document is not valid UTF-8")?;
                let canonical = logical.canonicalize()?;
                Ok(Self::from_state(
                    DocumentPath::Existing {
                        logical,
                        canonical,
                        original_len: metadata.len(),
                        original_modified: metadata.modified().ok(),
                        original_permissions: metadata.permissions(),
                    },
                    text,
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = logical
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let metadata = std::fs::metadata(parent).with_context(|| {
                    format!("new document parent does not exist: {}", parent.display())
                })?;
                ensure!(metadata.is_dir(), "new document parent is not a directory");
                Ok(Self::from_state(
                    DocumentPath::New { logical },
                    String::new(),
                ))
            }
            Err(error) => {
                Err(error).with_context(|| format!("cannot inspect {}", logical.display()))
            }
        }
    }

    fn from_state(path: DocumentPath, text: String) -> Self {
        let state = State {
            text,
            references: Vec::new(),
        };
        Self {
            path,
            saved_state: state.clone(),
            state,
            revision: 0,
            saved_revision: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            insert_start: None,
        }
    }

    pub fn path(&self) -> &DocumentPath {
        &self.path
    }
    pub fn text(&self) -> &str {
        &self.state.text
    }
    pub fn references(&self) -> &[ResolvedReference] {
        &self.state.references
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn saved_revision(&self) -> u64 {
        self.saved_revision
    }
    pub fn is_dirty(&self) -> bool {
        self.state != self.saved_state
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() || self.insert_start.is_some()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn snapshot(&self) -> DocumentSnapshot {
        DocumentSnapshot {
            text: self.state.text.clone(),
            references: self.state.references.clone(),
            revision: self.revision,
        }
    }

    pub fn mark_saved(&mut self) {
        self.saved_state = self.state.clone();
        self.saved_revision = self.revision;
    }

    pub fn begin_insert_group(&mut self) {
        if self.insert_start.is_none() {
            self.insert_start = Some(self.state.clone());
        }
    }

    pub fn end_insert_group(&mut self) {
        if let Some(before) = self.insert_start.take()
            && before != self.state
        {
            self.undo.push(before);
        }
    }

    pub fn apply(&mut self, edits: &[TextEdit]) -> Result<()> {
        if edits.is_empty() {
            return Ok(());
        }
        validate_edits(&self.state.text, edits)?;
        let before = self.state.clone();
        let mut references = map_references(&self.state.references, edits);
        let mut text = self.state.text.clone();
        for edit in edits.iter().rev() {
            let bytes = byte_range(&text, edit.range)?;
            text.replace_range(bytes, &edit.replacement);
        }
        references.sort_unstable_by_key(|reference| (reference.range.start, reference.range.end));
        self.state = State { text, references };
        if self.state != before {
            if self.insert_start.is_none() {
                self.undo.push(before);
            }
            self.redo.clear();
            self.bump_revision();
        }
        Ok(())
    }

    pub fn accept_reference(&mut self, accepted: AcceptedReference) -> Result<()> {
        ensure!(
            accepted.reference.friendly_text == accepted.replacement_text,
            "accepted reference friendly text does not match replacement"
        );
        let edit = TextEdit::new(
            accepted.replacement_range,
            accepted.replacement_text.clone(),
        );
        validate_edits(&self.state.text, std::slice::from_ref(&edit))?;
        let before = self.state.clone();
        let mut references = map_references(&self.state.references, std::slice::from_ref(&edit));
        let bytes = byte_range(&self.state.text, edit.range)?;
        let mut text = self.state.text.clone();
        text.replace_range(bytes, &edit.replacement);
        let start = accepted.replacement_range.start;
        let mut reference = accepted.reference;
        reference.range = TextRange {
            start,
            end: start + accepted.replacement_text.chars().count(),
        };
        ensure!(
            !references
                .iter()
                .any(|existing| ranges_overlap(existing.range, reference.range)),
            "accepted reference overlaps an existing reference"
        );
        references.push(reference);
        references.sort_unstable_by_key(|reference| reference.range.start);
        self.state = State { text, references };
        if self.state != before {
            if self.insert_start.is_none() {
                self.undo.push(before);
            }
            self.redo.clear();
            self.bump_revision();
        }
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        self.end_insert_group();
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo.push(std::mem::replace(&mut self.state, previous));
        self.bump_revision();
        true
    }

    pub fn redo(&mut self) -> bool {
        self.end_insert_group();
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.state, next));
        self.bump_revision();
        true
    }

    fn bump_revision(&mut self) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("document revision overflow");
    }
}

fn validate_edits(text: &str, edits: &[TextEdit]) -> Result<()> {
    let length = text.chars().count();
    let mut previous_end = 0;
    for (index, edit) in edits.iter().enumerate() {
        ensure!(edit.range.start <= edit.range.end, "edit range is reversed");
        ensure!(
            edit.range.end <= length,
            "edit range is outside the document"
        );
        if index > 0 {
            ensure!(
                edit.range.start >= previous_end,
                "edit ranges overlap or are unsorted"
            );
        }
        previous_end = edit.range.end;
    }
    Ok(())
}

fn map_references(references: &[ResolvedReference], edits: &[TextEdit]) -> Vec<ResolvedReference> {
    references
        .iter()
        .filter_map(|reference| {
            let original = reference.range;
            let mut shift: isize = 0;
            let mut replacement_len = original.end - original.start;
            for edit in edits {
                let inserted = edit.replacement.chars().count();
                let removed = edit.range.end - edit.range.start;
                if edit.range.end <= original.start {
                    shift += inserted as isize - removed as isize;
                } else if edit.range.start >= original.end {
                    continue;
                } else if edit.range == original && edit.replacement == reference.friendly_text {
                    replacement_len = inserted;
                } else {
                    return None;
                }
            }
            let start = original.start.checked_add_signed(shift)?;
            let mut mapped = reference.clone();
            mapped.range = TextRange {
                start,
                end: start + replacement_len,
            };
            Some(mapped)
        })
        .collect()
}

fn ranges_overlap(left: TextRange, right: TextRange) -> bool {
    left.start < right.end && right.start < left.end
}

fn byte_range(text: &str, range: TextRange) -> Result<std::ops::Range<usize>> {
    let start = char_to_byte(text, range.start).context("range start is outside the document")?;
    let end = char_to_byte(text, range.end).context("range end is outside the document")?;
    if start > end {
        bail!("range is reversed");
    }
    Ok(start..end)
}

fn char_to_byte(text: &str, index: usize) -> Option<usize> {
    if index == text.chars().count() {
        return Some(text.len());
    }
    text.char_indices().nth(index).map(|(byte, _)| byte)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::references::model::{
        ExternalUrlTarget, ReferenceId, ReferenceKind, ReferenceTarget,
    };
    use std::fs;

    fn range(start: usize, end: usize) -> TextRange {
        TextRange { start, end }
    }

    fn reference(id: u64, range: TextRange, friendly: &str) -> ResolvedReference {
        ResolvedReference {
            id: ReferenceId(id),
            range,
            friendly_text: friendly.into(),
            target: ReferenceTarget::ExternalUrl(ExternalUrlTarget {
                kind: ReferenceKind::GitHubIssue,
                url: format!("https://example.test/{id}"),
            }),
        }
    }

    fn accepted(id: u64, replacement_range: TextRange, friendly: &str) -> AcceptedReference {
        AcceptedReference {
            replacement_range,
            replacement_text: friendly.into(),
            reference: reference(id, range(0, 0), friendly),
        }
    }

    #[test]
    fn opens_existing_new_and_unnamed_documents_and_rejects_bad_inputs() {
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("task.md");
        fs::write(&existing, "héllo\n").unwrap();
        let opened = Document::open(&existing).unwrap();
        assert_eq!(opened.text(), "héllo\n");
        assert!(
            matches!(opened.path(), DocumentPath::Existing { logical, canonical, .. }
            if logical == &existing && canonical == &existing.canonicalize().unwrap())
        );
        assert!(!opened.is_dirty());

        let new_path = temp.path().join("new.md");
        let new = Document::open(&new_path).unwrap();
        assert!(matches!(new.path(), DocumentPath::New { logical } if logical == &new_path));
        assert_eq!(new.text(), "");
        assert!(matches!(Document::unnamed().path(), DocumentPath::Unnamed));
        fs::write(temp.path().join("bad.md"), [0xff]).unwrap();
        assert!(Document::open(temp.path().join("bad.md")).is_err());
        assert!(Document::open(temp.path().join("missing/new.md")).is_err());
        assert!(Document::open(temp.path()).is_err());
    }

    #[test]
    fn character_edits_are_unicode_safe_and_batches_use_original_coordinates() {
        let mut document = Document::from_state(DocumentPath::Unnamed, "aé日z".into());
        document
            .apply(&[
                TextEdit::new(range(1, 2), "É"),
                TextEdit::new(range(3, 3), "🙂"),
            ])
            .unwrap();
        assert_eq!(document.text(), "aÉ日🙂z");
        assert_eq!(document.revision(), 1);
        assert!(
            document
                .apply(&[TextEdit::new(range(99, 99), "x")])
                .is_err()
        );
        assert!(
            document
                .apply(&[
                    TextEdit::new(range(2, 3), "x"),
                    TextEdit::new(range(1, 2), "y"),
                ])
                .is_err()
        );
        assert!(
            document
                .apply(&[
                    TextEdit::new(range(1, 3), "x"),
                    TextEdit::new(range(2, 4), "y"),
                ])
                .is_err()
        );
    }

    #[test]
    fn edits_shift_at_start_preserve_at_end_and_invalidate_inside_or_overlap() {
        let scenarios = [
            (range(0, 0), "x", Some(range(3, 7))),
            (range(2, 2), "x", Some(range(3, 7))),
            (range(6, 6), "x", Some(range(2, 6))),
            (range(4, 4), "x", None),
            (range(1, 3), "", None),
            (range(5, 7), "", None),
            (range(2, 6), "@ref", Some(range(2, 6))),
            (range(2, 6), "plain", None),
        ];
        for (edit_range, replacement, expected) in scenarios {
            let mut document = Document::from_state(DocumentPath::Unnamed, "--@ref--".into());
            document
                .state
                .references
                .push(reference(1, range(2, 6), "@ref"));
            document.saved_state = document.state.clone();
            document
                .apply(&[TextEdit::new(edit_range, replacement)])
                .unwrap();
            assert_eq!(
                document.references().first().map(|item| item.range),
                expected
            );
        }
    }

    #[test]
    fn multiple_edits_accumulate_shifts_and_any_overlap_invalidates() {
        let mut document = Document::from_state(DocumentPath::Unnamed, "ab--@ref--yz".into());
        document
            .state
            .references
            .push(reference(1, range(4, 8), "@ref"));
        document.saved_state = document.state.clone();
        document
            .apply(&[
                TextEdit::new(range(0, 1), "αβ"),
                TextEdit::new(range(2, 2), "日"),
                TextEdit::new(range(10, 12), "z"),
            ])
            .unwrap();
        assert_eq!(document.references()[0].range, range(6, 10));

        document
            .apply(&[
                TextEdit::new(range(0, 0), "prefix"),
                TextEdit::new(range(7, 8), "x"),
            ])
            .unwrap();
        assert!(document.references().is_empty());
    }

    #[test]
    fn accepts_references_atomically_and_maps_existing_references() {
        let mut document = Document::from_state(DocumentPath::Unnamed, "@one and @tw".into());
        document
            .accept_reference(accepted(1, range(0, 4), "@one"))
            .unwrap();
        document
            .accept_reference(accepted(2, range(9, 12), "@two"))
            .unwrap();
        assert_eq!(document.text(), "@one and @two");
        assert_eq!(document.references()[0].range, range(0, 4));
        assert_eq!(document.references()[1].range, range(9, 13));
        let before = document.state.clone();
        let mut invalid = accepted(3, range(1, 3), "xx");
        invalid.reference.friendly_text = "different".into();
        assert!(document.accept_reference(invalid).is_err());
        assert_eq!(document.state, before);
    }

    #[test]
    fn compound_history_restores_text_and_references_and_revisions_never_decrease() {
        let mut document = Document::from_state(DocumentPath::Unnamed, "@r".into());
        document
            .accept_reference(accepted(1, range(0, 2), "@r"))
            .unwrap();
        document.mark_saved();
        assert_eq!(document.saved_revision(), 1);
        assert!(!document.is_dirty());
        document.begin_insert_group();
        document.apply(&[TextEdit::new(range(2, 2), "é")]).unwrap();
        document.apply(&[TextEdit::new(range(3, 3), "日")]).unwrap();
        document.end_insert_group();
        assert_eq!(document.text(), "@ré日");
        assert_eq!(document.revision(), 3);
        assert!(document.undo());
        assert_eq!(document.text(), "@r");
        assert_eq!(document.references().len(), 1);
        assert!(!document.is_dirty());
        assert_eq!(document.revision(), 4);
        assert!(document.redo());
        assert_eq!(document.text(), "@ré日");
        assert_eq!(document.revision(), 5);
    }

    #[test]
    fn editing_after_undo_branches_and_clears_redo() {
        let mut document = Document::unnamed();
        document.apply(&[TextEdit::new(range(0, 0), "a")]).unwrap();
        document.apply(&[TextEdit::new(range(1, 1), "b")]).unwrap();
        assert!(document.undo());
        assert!(document.can_redo());
        document.apply(&[TextEdit::new(range(1, 1), "c")]).unwrap();
        assert_eq!(document.text(), "ac");
        assert!(!document.can_redo());
        assert!(!document.redo());
    }

    #[test]
    fn immutable_lowering_is_end_to_start_and_preserves_friendly_state() {
        let mut document = Document::from_state(DocumentPath::Unnamed, "é @one / @two 日\n".into());
        document.state.references = vec![
            reference(1, range(2, 6), "@one"),
            reference(2, range(9, 13), "@two"),
        ];
        let snapshot = document.snapshot();
        let lowered = snapshot
            .lower(|item| Ok(format!("<{}>", item.id.0)))
            .unwrap();
        assert_eq!(lowered.text, "é <1> / <2> 日\n");
        assert_eq!(lowered.revision, 0);
        assert_eq!(snapshot.text(), "é @one / @two 日\n");
        assert_eq!(document.text(), "é @one / @two 日\n");
    }
}

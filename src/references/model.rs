use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GenerationId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReferenceId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceKind {
    GitFile,
    BroadFile,
    Symbol,
    Skill,
    GitHubIssue,
    GitHubPullRequest,
    JiraIssue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CandidateId {
    pub provider: ReferenceKind,
    pub opaque: String,
}

/// A half-open range measured in Unicode scalar values at the editor boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

impl TextRange {
    pub fn new(start: usize, end: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(start <= end, "reference range starts after it ends");
        Ok(Self { start, end })
    }
}

#[derive(Debug, Clone)]
pub struct QueryRequest {
    pub generation: GenerationId,
    pub query: String,
    pub scope: QueryScope,
    pub limit: usize,
    pub typed_leader: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryScope {
    Repository,
    File {
        canonical_path: PathBuf,
        relative_path: String,
        origin: FileOrigin,
    },
}

#[derive(Debug, Clone)]
pub struct ReferenceCandidate {
    pub id: CandidateId,
    pub generation: GenerationId,
    pub kind: ReferenceKind,
    pub friendly_text: String,
    pub display: CandidateDisplay,
    pub context_cost: ContextCost,
    pub file_context_cost: Option<ContextCost>,
    pub source_version: Option<FileVersion>,
}

#[derive(Debug, Clone, Default)]
pub struct CandidateDisplay {
    pub primary: String,
    pub secondary: Option<String>,
    pub location: Option<SourceLocation>,
    pub match_indices: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceLocation {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextCost {
    Pending,
    Tokens(usize),
    Bytes(u64),
    None,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileVersion {
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub content_sha256: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileOrigin {
    GitAware,
    Broad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTarget {
    pub canonical_path: PathBuf,
    pub relative_path: String,
    pub origin: FileOrigin,
    pub source_version: Option<FileVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SymbolIdentity {
    pub language: String,
    pub qualified_name: String,
    pub leaf_name: String,
    pub kind: String,
    pub is_definition: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolTarget {
    pub file: FileTarget,
    pub identity: SymbolIdentity,
    pub start_byte: usize,
    pub end_byte: usize,
    pub name_start_byte: usize,
    pub name_end_byte: usize,
    pub location: SourceLocation,
    pub markdown_anchor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillTarget {
    pub name: String,
    pub mention: String,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalUrlTarget {
    pub kind: ReferenceKind,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceTarget {
    File(FileTarget),
    Symbol(SymbolTarget),
    Skill(SkillTarget),
    ExternalUrl(ExternalUrlTarget),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedReference {
    pub id: ReferenceId,
    pub range: TextRange,
    pub friendly_text: String,
    pub target: ReferenceTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedTarget {
    pub target: ReferenceTarget,
    pub context_cost: ContextCost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub title: Option<String>,
    pub lines: Vec<PreviewLine>,
    pub highlighted_lines: Option<RangeInclusive<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewLine {
    pub number: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct AcceptedReference {
    pub replacement_range: TextRange,
    pub replacement_text: String,
    pub reference: ResolvedReference,
}

#[derive(Debug, Clone)]
pub struct LoweredReference {
    pub replacement: String,
    pub refreshed_target: ValidatedTarget,
}

#[derive(Debug, Clone)]
pub struct CompletionActivation {
    pub kind: ReferenceKind,
    pub replacement_range: TextRange,
    pub query: String,
    pub scope: QueryScope,
    pub typed_leader: String,
}

#[derive(Debug, Clone, Default)]
pub struct SessionUpdate {
    pub candidates_changed: bool,
    pub completed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QueryEmission {
    pub generation: GenerationId,
    pub candidates: Vec<ReferenceCandidate>,
    pub completed: bool,
}

pub(crate) type SharedProvider = Arc<dyn super::ReferenceProvider>;

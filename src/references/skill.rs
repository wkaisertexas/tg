mod discovery;
mod metadata;
mod roots;
#[cfg(test)]
mod tests;

use super::model::{
    CandidateDisplay, CandidateId, ContextCost, Preview, PreviewLine, QueryRequest, QueryScope,
    ReferenceCandidate, ReferenceKind, ReferenceTarget, SkillTarget, ValidatedTarget,
};
use super::{CancellationFlag, ReferenceProvider};
use crate::config::SkillsConfig;
use anyhow::{Context, Result, bail, ensure};
pub use discovery::SkillDiscoveryEnvironment;
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use metadata::parse_frontmatter;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct SkillRecord {
    name: String,
    description: String,
    display_name: String,
    short_description: String,
    scope: String,
    metadata_path: PathBuf,
    canonical_metadata: PathBuf,
    mention: String,
    name_key: String,
    description_key: String,
}

impl SkillRecord {
    fn target(&self) -> ReferenceTarget {
        ReferenceTarget::Skill(SkillTarget {
            name: self.name.clone(),
            mention: self.mention.clone(),
            source_path: self.metadata_path.clone(),
        })
    }
}

#[derive(Debug)]
pub struct SkillProvider {
    skills: Vec<SkillRecord>,
    diagnostics: Vec<String>,
}

impl SkillProvider {
    pub fn new(
        repository_root: &Path,
        working_directory: &Path,
        config: &SkillsConfig,
        leader: impl Into<String>,
    ) -> Result<Self> {
        Self::with_environment(
            SkillDiscoveryEnvironment::local(repository_root, working_directory),
            config,
            leader,
        )
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
    pub fn len(&self) -> usize {
        self.skills.len()
    }
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    fn record_by_id(&self, id: &CandidateId) -> Result<&SkillRecord> {
        ensure!(
            id.provider == ReferenceKind::Skill,
            "candidate belongs to another provider"
        );
        self.skills
            .iter()
            .find(|skill| opaque_id(skill) == id.opaque)
            .context("skill candidate is no longer available")
    }
}

impl ReferenceProvider for SkillProvider {
    fn kind(&self) -> ReferenceKind {
        ReferenceKind::Skill
    }

    fn query(
        &self,
        request: QueryRequest,
        cancellation: &CancellationFlag,
    ) -> Result<Vec<ReferenceCandidate>> {
        ensure!(
            request.scope == QueryScope::Repository,
            "skill provider only supports repository queries"
        );
        let matcher = SkimMatcherV2::default().ignore_case();
        let query = request.query.to_ascii_lowercase();
        let mut found = self
            .skills
            .iter()
            .filter_map(|skill| {
                if cancellation.is_cancelled() {
                    return None;
                }
                let name = skill.name.to_ascii_lowercase();
                let score = if query.is_empty() {
                    0
                } else if name == query {
                    1_000_000
                } else if name.starts_with(&query) {
                    500_000
                } else {
                    matcher
                        .fuzzy_match(&skill.name, &request.query)
                        .or_else(|| matcher.fuzzy_match(&skill.display_name, &request.query))
                        .or_else(|| matcher.fuzzy_match(&skill.description, &request.query))?
                };
                Some((score, skill))
            })
            .collect::<Vec<_>>();
        found.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.scope.cmp(&right.scope))
                .then_with(|| left.metadata_path.cmp(&right.metadata_path))
        });
        Ok(found
            .into_iter()
            .take(request.limit)
            .map(|(_, skill)| ReferenceCandidate {
                id: CandidateId {
                    provider: ReferenceKind::Skill,
                    opaque: opaque_id(skill),
                },
                generation: request.generation,
                kind: ReferenceKind::Skill,
                friendly_text: skill.mention.clone(),
                display: CandidateDisplay {
                    primary: skill.display_name.clone(),
                    secondary: Some(format!(
                        "{} · {} · {}",
                        skill.short_description,
                        skill.scope,
                        skill.metadata_path.display()
                    )),
                    ..CandidateDisplay::default()
                },
                context_cost: ContextCost::None,
                file_context_cost: None,
                source_version: None,
                token_source: None,
            })
            .collect())
    }

    fn resolve(&self, id: &CandidateId) -> Result<ReferenceTarget> {
        Ok(self.record_by_id(id)?.target())
    }

    fn validate(&self, target: &ReferenceTarget) -> Result<ValidatedTarget> {
        let ReferenceTarget::Skill(target) = target else {
            bail!("skill provider cannot validate this target")
        };
        let canonical = target
            .source_path
            .canonicalize()
            .context("skill metadata no longer exists")?;
        let skill = self
            .skills
            .iter()
            .find(|skill| skill.canonical_metadata == canonical)
            .context("skill is no longer available")?;
        let current = parse_frontmatter(
            &fs::read_to_string(&canonical)?,
            &skill.name_key,
            &skill.description_key,
        )?;
        ensure!(current.0 == skill.name, "skill name changed");
        Ok(ValidatedTarget {
            target: skill.target(),
            context_cost: ContextCost::None,
        })
    }

    fn lower(&self, target: &ValidatedTarget) -> Result<String> {
        let ReferenceTarget::Skill(skill) = &target.target else {
            bail!("skill provider cannot lower this target")
        };
        Ok(skill.mention.clone())
    }

    fn preview(&self, target: &ReferenceTarget) -> Result<Option<Preview>> {
        let ReferenceTarget::Skill(target) = target else {
            bail!("skill provider cannot preview this target")
        };
        let canonical = target.source_path.canonicalize()?;
        let skill = self
            .skills
            .iter()
            .find(|skill| skill.canonical_metadata == canonical)
            .context("skill is no longer available")?;
        Ok(Some(Preview {
            title: Some(skill.display_name.clone()),
            lines: [
                format!("Name: {}", skill.name),
                format!("Description: {}", skill.short_description),
                format!("Scope: {}", skill.scope),
                format!("Source: {}", skill.metadata_path.display()),
            ]
            .into_iter()
            .map(|text| PreviewLine { number: None, text })
            .collect(),
            highlighted_lines: None,
        }))
    }

    fn context_cost(&self, _target: &ReferenceTarget) -> Result<ContextCost> {
        Ok(ContextCost::None)
    }
}

fn opaque_id(skill: &SkillRecord) -> String {
    skill.canonical_metadata.to_string_lossy().into_owned()
}

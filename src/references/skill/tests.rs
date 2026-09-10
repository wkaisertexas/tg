use super::*;
use crate::config::{SkillDiscovery, SkillRootConfig};
use crate::references::model::GenerationId;

mod discovery;
mod provider;

fn write_skill(root: &Path, directory: &str, name: &str, description: &str) -> PathBuf {
    let directory = root.join(directory);
    fs::create_dir_all(&directory).unwrap();
    let metadata = directory.join("SKILL.md");
    fs::write(
        &metadata,
        format!("---\nname: {name}\ndescription: {description}\n---\n# ignored body\n"),
    )
    .unwrap();
    metadata
}

fn environment(temp: &Path) -> SkillDiscoveryEnvironment {
    let repository = temp.join("repo");
    let working = repository.join("nested/work");
    fs::create_dir_all(&working).unwrap();
    for directory in ["home", "codex", "admin"] {
        fs::create_dir_all(temp.join(directory)).unwrap();
    }
    SkillDiscoveryEnvironment {
        repository_root: repository,
        working_directory: working,
        home: Some(temp.join("home")),
        codex_home: Some(temp.join("codex")),
        admin_skills: Some(temp.join("admin")),
        codex_config: Some(temp.join("codex/config.toml")),
    }
}

fn request(query: &str) -> QueryRequest {
    QueryRequest {
        generation: GenerationId(9),
        query: query.into(),
        scope: QueryScope::Repository,
        limit: 100,
        typed_leader: "$".into(),
    }
}

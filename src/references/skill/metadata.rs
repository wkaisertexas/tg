use anyhow::{Context, Result, ensure};
use serde_yaml::Value;
use std::fs;
use std::path::Path;

pub(super) fn parse_frontmatter(
    source: &str,
    name_key: &str,
    description_key: &str,
) -> Result<(String, String)> {
    let mut lines = source.lines();
    ensure!(lines.next() == Some("---"), "missing YAML frontmatter");
    let mut yaml = String::new();
    let mut closed = false;
    for line in lines {
        if line == "---" {
            closed = true;
            break;
        }
        yaml.push_str(line);
        yaml.push('\n');
    }
    ensure!(closed, "unterminated YAML frontmatter");
    let value: Value = serde_yaml::from_str(&yaml)?;
    let name = yaml_string(&value, name_key).context("missing skill name")?;
    let description = yaml_string(&value, description_key).context("missing skill description")?;
    ensure!(!name.trim().is_empty(), "skill name is empty");
    ensure!(!description.trim().is_empty(), "skill description is empty");
    Ok((name, description))
}

fn yaml_string(value: &Value, key: &str) -> Option<String> {
    let value = key.split('.').try_fold(value, |value, segment| {
        value.as_mapping()?.get(Value::String(segment.into()))
    })?;
    value.as_str().map(str::to_owned)
}

pub(super) fn interface_metadata(metadata: &Path) -> Option<(String, String)> {
    let path = metadata.parent()?.join("agents/openai.yaml");
    let value: Value = serde_yaml::from_str(&fs::read_to_string(path).ok()?).ok()?;
    Some((
        yaml_string(&value, "interface.display_name")?,
        yaml_string(&value, "interface.short_description")?,
    ))
}

pub(super) fn render_mention(template: &str, leader: &str, name: &str) -> String {
    template
        .replace("$${", "\0{")
        .replace("${leader}", leader)
        .replace("${name}", name)
        .replace("\0{", "${")
}

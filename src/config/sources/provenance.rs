use super::*;

pub(super) fn record_provenance(
    value: &toml::Value,
    prefix: &str,
    kind: SourceKind,
    path: Option<&Path>,
    provenance: &mut BTreeMap<String, LoadedSource>,
) {
    if let toml::Value::Table(table) = value {
        for (key, value) in table {
            let child = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            record_provenance(value, &child, kind, path, provenance);
        }
    } else {
        provenance.insert(
            prefix.into(),
            LoadedSource {
                kind,
                path: path.map(Path::to_path_buf),
            },
        );
        if let toml::Value::Array(values) = value {
            for (index, value) in values.iter().enumerate() {
                record_provenance(value, &format!("{prefix}[{index}]"), kind, path, provenance);
            }
        }
    }
}

pub(super) fn record_environment_provenance(
    inputs: &ConfigInputs,
    provenance: &mut BTreeMap<String, LoadedSource>,
) {
    for (variable, key) in [
        ("TG_TOKENIZER", "tokens.tokenizer"),
        ("TG_GH_COMMAND", "providers.github.command"),
        ("TG_JIRA_COMMAND", "providers.jira.command"),
        ("NO_COLOR", "ui.color"),
    ] {
        if inputs.env(variable).is_some() {
            provenance.insert(
                key.into(),
                LoadedSource {
                    kind: SourceKind::Environment,
                    path: None,
                },
            );
        }
    }
}

pub(super) fn record_cli_provenance(
    cli: &CliConfigOverrides,
    provenance: &mut BTreeMap<String, LoadedSource>,
) {
    for (changed, key) in [
        (cli.tokenizer.is_some(), "tokens.tokenizer"),
        (cli.no_preview, "ui.preview"),
        (cli.no_color, "ui.color"),
    ] {
        if changed {
            provenance.insert(
                key.into(),
                LoadedSource {
                    kind: SourceKind::Cli,
                    path: None,
                },
            );
        }
    }
}

pub(super) fn source_precedence(kind: SourceKind) -> u8 {
    match kind {
        SourceKind::User => 1,
        SourceKind::Project => 2,
        SourceKind::Environment => 3,
        SourceKind::Cli => 4,
    }
}

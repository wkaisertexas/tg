use super::*;
use sha2::{Digest, Sha256};

pub(super) fn identity_for(path: &Path, symbol: &Symbol) -> SymbolIdentity {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown")
        .to_ascii_lowercase();
    let language = if matches!(extension.as_str(), "md" | "markdown") {
        "markdown".into()
    } else {
        extension
    };
    SymbolIdentity {
        language,
        qualified_name: symbol.qualified_name.clone(),
        leaf_name: symbol.leaf_name.clone(),
        kind: symbol.kind.clone(),
        is_definition: symbol.is_definition,
    }
}

pub(super) fn target_for(entry: &IndexedSymbol) -> SymbolTarget {
    SymbolTarget {
        file: FileTarget {
            canonical_path: entry.canonical_path.clone(),
            relative_path: entry.relative_path.clone(),
            origin: entry.origin,
            source_version: Some(entry.source_version.clone()),
        },
        identity: identity_for(&entry.canonical_path, &entry.symbol),
        start_byte: entry.symbol.range_start_byte,
        end_byte: entry.symbol.range_end_byte,
        name_start_byte: entry.symbol.name_start_byte,
        name_end_byte: entry.symbol.name_end_byte,
        location: SourceLocation {
            line: entry.symbol.start.line,
            column: entry.symbol.start.column,
        },
        markdown_anchor: language::is_markdown_symbol(&entry.symbol)
            .then(|| language::markdown_slug(&entry.symbol.leaf_name)),
    }
}

pub(super) fn stable_candidate_id(target: &SymbolTarget) -> String {
    let mut digest = Sha256::new();
    digest.update(target.file.canonical_path.to_string_lossy().as_bytes());
    digest.update(target.file.relative_path.as_bytes());
    digest.update([match target.file.origin {
        FileOrigin::GitAware => 0,
        FileOrigin::Broad => 1,
    }]);
    digest.update(target.identity.language.as_bytes());
    digest.update(target.identity.qualified_name.as_bytes());
    digest.update(target.identity.leaf_name.as_bytes());
    digest.update(target.identity.kind.as_bytes());
    digest.update([u8::from(target.identity.is_definition)]);
    digest.update(target.start_byte.to_le_bytes());
    digest.update(target.end_byte.to_le_bytes());
    if let Some(version) = &target.file.source_version {
        digest.update(version.content_sha256);
    }
    format!("{:x}", digest.finalize())
}

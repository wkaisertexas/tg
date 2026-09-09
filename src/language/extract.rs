use super::classify::{
    classification, go_package_binding_kind, is_callable_variable, is_go_field_name,
};
use super::grammar::Flavor;
use super::{SourcePoint, Symbol};
use std::sync::OnceLock;
use tree_sitter::Node;

pub(super) fn symbols(node: Node<'_>, source: &[u8], flavor: Flavor) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    if flavor == Flavor::Bash {
        visit_bash(node, source, &mut Vec::new(), &mut symbols);
    } else {
        visit(node, source, flavor, &mut Vec::new(), &mut symbols);
    }
    symbols
}

fn visit_bash(
    node: Node<'_>,
    source: &[u8],
    function_scopes: &mut Vec<String>,
    out: &mut Vec<Symbol>,
) {
    let original_scope_len = function_scopes.len();
    if node.kind() == "function_definition"
        && let Some(name_node) = node.child_by_field_name("name")
        && name_node.kind() == "word"
        && let Ok(name) = name_node.utf8_text(source)
        && !name.is_empty()
    {
        let qualified_name = if function_scopes.is_empty() {
            name.to_owned()
        } else {
            format!("{}::{name}", function_scopes.join("::"))
        };
        out.push(Symbol {
            leaf_name: name.to_owned(),
            qualified_name,
            kind: "function".to_owned(),
            start: name_node.start_position().into(),
            name_start_byte: name_node.start_byte(),
            name_end_byte: name_node.end_byte(),
            range_start_byte: node.start_byte(),
            range_end_byte: node.end_byte(),
            is_definition: true,
        });
        function_scopes.push(name.to_owned());
    } else if function_scopes.is_empty()
        && node.kind() == "variable_assignment"
        && let Some(name_node) = node.child_by_field_name("name")
        && name_node.kind() == "variable_name"
        && is_top_level_shell_assignment(node)
        && let Ok(name) = name_node.utf8_text(source)
        && !name.is_empty()
    {
        let declaration = node
            .parent()
            .filter(|parent| parent.kind() == "declaration_command")
            .unwrap_or(node);
        out.push(Symbol {
            leaf_name: name.to_owned(),
            qualified_name: name.to_owned(),
            kind: "variable".to_owned(),
            start: name_node.start_position().into(),
            name_start_byte: name_node.start_byte(),
            name_end_byte: name_node.end_byte(),
            range_start_byte: declaration.start_byte(),
            range_end_byte: declaration.end_byte(),
            is_definition: true,
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit_bash(child, source, function_scopes, out);
    }
    function_scopes.truncate(original_scope_len);
}

fn is_top_level_shell_assignment(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "program"
        || (parent.kind() == "declaration_command"
            && parent
                .parent()
                .is_some_and(|grandparent| grandparent.kind() == "program"))
}

pub(super) fn supplement_c_family_declarations(source: &str, symbols: &mut Vec<Symbol>) -> bool {
    static DECLARATION: OnceLock<regex::Regex> = OnceLock::new();
    let declaration = DECLARATION.get_or_init(|| {
        regex::Regex::new(r"\b(class|struct|union|enum)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap()
    });
    let original_len = symbols.len();
    let mut added_offsets = Vec::new();
    let mut byte_offset = 0;
    for (row, line) in source.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("//") && !trimmed.starts_with("/*") && !trimmed.starts_with('*') {
            for captures in declaration.captures_iter(line) {
                let whole = captures.get(0).unwrap();
                let name = captures.get(2).unwrap();
                if line[..whole.start()].trim_end().ends_with("template<") {
                    continue;
                }
                let name_start_byte = byte_offset + name.start();
                let name_end = byte_offset + name.end();
                if symbols[..original_len]
                    .binary_search_by_key(&name_start_byte, |symbol| symbol.name_start_byte)
                    .is_ok()
                    || added_offsets.contains(&name_start_byte)
                {
                    continue;
                }
                added_offsets.push(name_start_byte);
                let kind = captures.get(1).unwrap().as_str();
                let tail = &source.as_bytes()[name_end..(name_end + 500).min(source.len())];
                let brace = tail.iter().position(|byte| *byte == b'{');
                let semicolon = tail.iter().position(|byte| *byte == b';');
                let line_without_newline = line.trim_end_matches(['\r', '\n']);
                let leading = line_without_newline.len() - line_without_newline.trim_start().len();
                symbols.push(Symbol {
                    leaf_name: name.as_str().to_owned(),
                    qualified_name: name.as_str().to_owned(),
                    kind: kind.to_owned(),
                    start: SourcePoint {
                        line: row + 1,
                        column: name.start() + 1,
                    },
                    name_start_byte,
                    name_end_byte: name_end,
                    range_start_byte: byte_offset + leading,
                    range_end_byte: byte_offset + line_without_newline.len(),
                    is_definition: brace
                        .is_some_and(|brace| semicolon.is_none_or(|semicolon| brace < semicolon)),
                });
            }
        }
        byte_offset += line.len();
    }
    !added_offsets.is_empty()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    Module,
    Type,
    Interface,
}
struct Scope {
    name: String,
    kind: ScopeKind,
    absolute: bool,
}

fn scope_kind(kind: &str) -> Option<ScopeKind> {
    match kind {
        "namespace_definition" | "mod_item" | "internal_module" | "module" => {
            Some(ScopeKind::Module)
        }
        "class_definition"
        | "class_specifier"
        | "class"
        | "class_declaration"
        | "abstract_class_declaration"
        | "interface_declaration"
        | "record_declaration"
        | "annotation_type_declaration"
        | "struct_specifier"
        | "struct_item"
        | "union_specifier"
        | "union_item"
        | "enum_specifier"
        | "enum_item"
        | "enum_declaration"
        | "enum_constant"
        | "trait_item" => Some(ScopeKind::Type),
        _ => None,
    }
}

fn identifier<'a>(node: Node<'a>, preferred: &str) -> Option<Node<'a>> {
    let direct = node
        .child_by_field_name(preferred)
        .or_else(|| node.child_by_field_name("name"));
    direct
        .and_then(unwrap_identifier)
        .or_else(|| unwrap_identifier(node))
}

fn unwrap_identifier(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(
        node.kind(),
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "property_identifier"
            | "private_property_identifier"
            | "namespace_identifier"
            | "constant"
            | "operator_name"
            | "operator"
            | "setter"
    ) {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find_map(unwrap_identifier)
}

struct ExtractedName<'a> {
    node: Node<'a>,
    leaf: String,
    explicit_qualified: Option<String>,
}

fn extracted_name<'a>(
    node: Node<'a>,
    preferred: &str,
    flavor: Flavor,
    source: &[u8],
) -> Option<ExtractedName<'a>> {
    let direct = node
        .child_by_field_name(preferred)
        .or_else(|| node.child_by_field_name("name"));
    let uses_full_name = (matches!(flavor, Flavor::CSharp)
        && matches!(
            node.kind(),
            "namespace_declaration" | "file_scoped_namespace_declaration"
        ))
        || (matches!(flavor, Flavor::Ruby)
            && matches!(node.kind(), "class" | "module")
            && direct.is_some_and(|name| name.kind() == "scope_resolution"));
    if uses_full_name {
        let direct = direct?;
        let raw = direct.utf8_text(source).ok()?;
        let qualified = normalize_qualified(raw, flavor);
        let leaf = qualified.rsplit("::").next()?.to_owned();
        return Some(ExtractedName {
            node: last_identifier(direct).unwrap_or(direct),
            leaf,
            explicit_qualified: Some(qualified),
        });
    }
    let name_node = if matches!(flavor, Flavor::Ruby)
        && matches!(node.kind(), "method" | "singleton_method" | "alias")
    {
        direct?
    } else {
        identifier(node, preferred)?
    };
    let mut leaf = name_node.utf8_text(source).ok()?.to_owned();
    if node.kind() == "alias" {
        leaf = leaf.trim_start_matches(':').to_owned();
    }
    let explicit_qualified = if matches!(flavor, Flavor::Ruby)
        && node.kind() == "singleton_method"
        && let Some(object) = node.child_by_field_name("object")
        && object.kind() != "self"
    {
        let object = normalize_qualified(object.utf8_text(source).ok()?, flavor);
        Some(format!("{object}::{leaf}"))
    } else {
        None
    };
    (!leaf.is_empty()).then_some(ExtractedName {
        node: name_node,
        leaf,
        explicit_qualified,
    })
}

fn normalize_qualified(raw: &str, flavor: Flavor) -> String {
    let compact: String = raw
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if matches!(flavor, Flavor::CSharp) {
        compact.replace('.', "::")
    } else {
        compact.trim_start_matches("::").to_owned()
    }
}

fn last_identifier(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(
        node.kind(),
        "identifier" | "constant" | "namespace_identifier"
    ) {
        return Some(node);
    }
    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    children.into_iter().rev().find_map(last_identifier)
}

fn scope_prefix(scopes: &[Scope]) -> String {
    let start = scopes.iter().rposition(|scope| scope.absolute).unwrap_or(0);
    scopes[start..]
        .iter()
        .map(|scope| scope.name.as_str())
        .collect::<Vec<_>>()
        .join("::")
}

fn qualify(scopes: &[Scope], name: &str, explicit: Option<&str>) -> String {
    if let Some(explicit) = explicit {
        return explicit.to_owned();
    }
    let prefix = scope_prefix(scopes);
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}::{name}")
    }
}

fn visit(
    node: Node<'_>,
    source: &[u8],
    flavor: Flavor,
    scopes: &mut Vec<Scope>,
    out: &mut Vec<Symbol>,
) {
    let inside_type = scopes
        .last()
        .is_some_and(|scope| matches!(scope.kind, ScopeKind::Type | ScopeKind::Interface));
    let original_scope_len = scopes.len();
    if matches!(flavor, Flavor::Go)
        && node.kind() == "method_declaration"
        && let Some(name_node) = go_receiver_name(node)
        && let Ok(name) = name_node.utf8_text(source)
    {
        scopes.push(Scope {
            name: name.to_owned(),
            kind: ScopeKind::Type,
            absolute: false,
        });
    }
    if node.kind() == "impl_item"
        && let Some(type_node) = node.child_by_field_name("type").and_then(unwrap_identifier)
        && let Ok(name) = type_node.utf8_text(source)
    {
        scopes.push(Scope {
            name: name.to_owned(),
            kind: ScopeKind::Type,
            absolute: false,
        });
    }
    if matches!(flavor, Flavor::CSharp) && node.kind() == "field_declaration" {
        emit_csharp_fields(node, source, scopes, out);
    }
    if matches!(flavor, Flavor::Ruby) && node.kind() == "assignment" {
        emit_ruby_constant(node, source, scopes, out);
    }
    if let Some((symbol_kind, field)) = classification(node, flavor, inside_type)
        && let Some(name) = extracted_name(node, field, flavor, source)
        && !(node.kind() == "method_definition" && name.leaf == "constructor")
    {
        let declaration_node = if matches!(flavor, Flavor::Go) && is_go_field_name(node) {
            node.parent().expect("Go field parent checked above")
        } else if matches!(flavor, Flavor::Go) && go_package_binding_kind(node).is_some() {
            node.parent().expect("Go binding parent checked above")
        } else {
            node
        };
        let qualified_name = qualify(scopes, &name.leaf, name.explicit_qualified.as_deref());
        out.push(Symbol {
            leaf_name: name.leaf.clone(),
            qualified_name,
            kind: symbol_kind.to_owned(),
            start: name.node.start_position().into(),
            name_start_byte: name.node.start_byte(),
            name_end_byte: name.node.end_byte(),
            range_start_byte: declaration_node.start_byte(),
            range_end_byte: declaration_node.end_byte(),
            is_definition: is_definition(node, flavor, scopes),
        });
        if let Some(kind) = language_scope_kind(node, flavor).or_else(|| scope_kind(node.kind())) {
            scopes.push(Scope {
                name: name
                    .explicit_qualified
                    .clone()
                    .unwrap_or_else(|| name.leaf.clone()),
                kind,
                absolute: name.explicit_qualified.is_some(),
            });
        }
    }
    if is_callable(node, flavor) {
        scopes.truncate(original_scope_len);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit(child, source, flavor, scopes, out);
        // A file-scoped namespace is a sibling of the declarations it governs,
        // so retain its scope for the remaining compilation-unit children.
        if matches!(flavor, Flavor::CSharp)
            && child.kind() == "file_scoped_namespace_declaration"
            && let Some(name) = extracted_name(child, "name", flavor, source)
        {
            scopes.push(Scope {
                name: name
                    .explicit_qualified
                    .clone()
                    .unwrap_or_else(|| name.leaf.clone()),
                kind: ScopeKind::Module,
                absolute: name.explicit_qualified.is_some(),
            });
        }
    }
    scopes.truncate(original_scope_len);
}

fn emit_csharp_fields(node: Node<'_>, source: &[u8], scopes: &[Scope], out: &mut Vec<Symbol>) {
    fn collect_declarators<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
        if node.kind() == "variable_declarator" {
            out.push(node);
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect_declarators(child, out);
        }
    }
    let mut declarators = Vec::new();
    collect_declarators(node, &mut declarators);
    for declarator in declarators {
        let Some(name_node) = declarator
            .child_by_field_name("name")
            .and_then(unwrap_identifier)
        else {
            continue;
        };
        let Ok(name) = name_node.utf8_text(source) else {
            continue;
        };
        out.push(Symbol {
            leaf_name: name.to_owned(),
            qualified_name: qualify(scopes, name, None),
            kind: "field".into(),
            start: name_node.start_position().into(),
            name_start_byte: name_node.start_byte(),
            name_end_byte: name_node.end_byte(),
            range_start_byte: node.start_byte(),
            range_end_byte: node.end_byte(),
            is_definition: true,
        });
    }
}

fn emit_ruby_constant(node: Node<'_>, source: &[u8], scopes: &[Scope], out: &mut Vec<Symbol>) {
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    if !matches!(left.kind(), "constant" | "scope_resolution") {
        return;
    }
    let Ok(raw) = left.utf8_text(source) else {
        return;
    };
    let explicit =
        (left.kind() == "scope_resolution").then(|| normalize_qualified(raw, Flavor::Ruby));
    let leaf = explicit
        .as_deref()
        .unwrap_or(raw)
        .rsplit("::")
        .next()
        .unwrap_or(raw);
    let name_node = last_identifier(left).unwrap_or(left);
    out.push(Symbol {
        leaf_name: leaf.to_owned(),
        qualified_name: qualify(scopes, leaf, explicit.as_deref()),
        kind: "constant".into(),
        start: name_node.start_position().into(),
        name_start_byte: name_node.start_byte(),
        name_end_byte: name_node.end_byte(),
        range_start_byte: node.start_byte(),
        range_end_byte: node.end_byte(),
        is_definition: true,
    });
}

fn language_scope_kind(node: Node<'_>, flavor: Flavor) -> Option<ScopeKind> {
    if matches!(flavor, Flavor::CSharp) {
        return match node.kind() {
            "namespace_declaration" | "file_scoped_namespace_declaration" => {
                Some(ScopeKind::Module)
            }
            "interface_declaration" => Some(ScopeKind::Interface),
            "class_declaration" | "struct_declaration" | "record_declaration"
            | "enum_declaration" => Some(ScopeKind::Type),
            _ => None,
        };
    }
    if matches!(flavor, Flavor::Ruby) {
        return match node.kind() {
            "module" => Some(ScopeKind::Module),
            "class" => Some(ScopeKind::Type),
            _ => None,
        };
    }
    (matches!(flavor, Flavor::Go)
        && node.kind() == "type_spec"
        && node
            .child_by_field_name("type")
            .is_some_and(|node| matches!(node.kind(), "struct_type" | "interface_type")))
    .then_some(ScopeKind::Type)
}

fn go_receiver_name(node: Node<'_>) -> Option<Node<'_>> {
    fn find_type_identifier(node: Node<'_>) -> Option<Node<'_>> {
        if node.kind() == "type_identifier" {
            return Some(node);
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find_map(find_type_identifier)
    }
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    receiver.named_children(&mut cursor).find_map(|parameter| {
        parameter
            .child_by_field_name("type")
            .and_then(find_type_identifier)
    })
}

fn is_callable(node: Node<'_>, flavor: Flavor) -> bool {
    if matches!(flavor, Flavor::Ruby)
        && matches!(
            node.kind(),
            "method" | "singleton_method" | "lambda" | "block" | "do_block"
        )
    {
        return true;
    }
    matches!(
        node.kind(),
        "function_definition"
            | "function_item"
            | "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "arrow_function"
            | "function_signature"
            | "method_declaration"
            | "method_elem"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "annotation_type_element_declaration"
            | "method_definition"
            | "method_signature"
            | "abstract_method_signature"
    ) || (node.kind() == "variable_declarator" && is_callable_variable(node))
}

fn is_definition(node: Node<'_>, flavor: Flavor, scopes: &[Scope]) -> bool {
    if matches!(flavor, Flavor::Ruby) {
        return true;
    }
    if matches!(flavor, Flavor::CSharp) {
        return match node.kind() {
            "method_declaration" | "constructor_declaration" => {
                node.child_by_field_name("body").is_some()
            }
            "property_declaration" => !scopes
                .last()
                .is_some_and(|scope| scope.kind == ScopeKind::Interface),
            _ => true,
        };
    }
    match node.kind() {
        "class_specifier"
        | "struct_specifier"
        | "union_specifier"
        | "enum_specifier"
        | "function_signature"
        | "method_elem"
        | "annotation_type_element_declaration"
        | "method_signature"
        | "abstract_method_signature"
        | "property_signature" => false,
        "function_declaration"
        | "generator_function_declaration"
        | "method_definition"
        | "method_declaration" => node.child_by_field_name("body").is_some(),
        _ => true,
    }
}

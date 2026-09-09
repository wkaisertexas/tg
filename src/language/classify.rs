use super::grammar::Flavor;
use tree_sitter::Node;

pub(super) fn classification(
    node: Node<'_>,
    flavor: Flavor,
    inside_type: bool,
) -> Option<(&'static str, &'static str)> {
    let kind = node.kind();
    let is_ecmascript = matches!(
        flavor,
        Flavor::JavaScript | Flavor::TypeScript | Flavor::Tsx
    );
    let is_typescript = matches!(flavor, Flavor::TypeScript | Flavor::Tsx);
    let is_go = matches!(flavor, Flavor::Go);
    let is_java = matches!(flavor, Flavor::Java);
    let is_csharp = matches!(flavor, Flavor::CSharp);
    let is_ruby = matches!(flavor, Flavor::Ruby);
    Some(match kind {
        "namespace_definition" | "mod_item" => ("module", "name"),
        "namespace_declaration" | "file_scoped_namespace_declaration" if is_csharp => {
            ("module", "name")
        }
        "internal_module" | "module" if is_typescript => ("module", "name"),
        "module" if is_ruby => ("module", "name"),
        "class_definition" => ("class", "name"),
        "class" if is_ruby => ("class", "name"),
        "class_declaration" if is_csharp || is_ecmascript || is_java => ("class", "name"),
        "class" if is_ecmascript && node.child_by_field_name("name").is_some() => ("class", "name"),
        "abstract_class_declaration" if is_typescript => ("class", "name"),
        "interface_declaration" if is_typescript || is_java || is_csharp => ("interface", "name"),
        "record_declaration" if is_java || is_csharp => ("record", "name"),
        "struct_declaration" if is_csharp => ("struct", "name"),
        "delegate_declaration" if is_csharp => ("delegate", "name"),
        "annotation_type_declaration" if is_java => ("annotation", "name"),
        "class_specifier" => ("class", "name"),
        "struct_specifier" | "struct_item" => ("struct", "name"),
        "union_specifier" | "union_item" => ("union", "name"),
        "enum_specifier" | "enum_item" => ("enum", "name"),
        "enum_declaration" if is_typescript || is_java || is_csharp => ("enum", "name"),
        "type_spec" if is_go => {
            let symbol_kind = match node.child_by_field_name("type").map(|node| node.kind()) {
                Some("struct_type") => "struct",
                Some("interface_type") => "interface",
                _ => "type",
            };
            (symbol_kind, "name")
        }
        "type_alias" if is_go => ("type alias", "name"),
        "trait_item" => ("trait", "name"),
        "type_item" | "type_definition" | "alias_declaration" => ("type alias", "name"),
        "type_alias_declaration" if is_typescript => ("type alias", "name"),
        "function_item" | "function_definition" => {
            (if inside_type { "method" } else { "function" }, "name")
        }
        "function_declaration" | "generator_function_declaration" if is_ecmascript => {
            (if inside_type { "method" } else { "function" }, "name")
        }
        "function_expression" | "generator_function"
            if is_ecmascript && node.child_by_field_name("name").is_some() =>
        {
            (if inside_type { "method" } else { "function" }, "name")
        }
        "function_signature" if is_typescript => {
            (if inside_type { "method" } else { "function" }, "name")
        }
        "function_declaration" if is_go => ("function", "name"),
        "method_declaration" if is_go || is_java => ("method", "name"),
        "method_declaration" | "constructor_declaration" if is_csharp => ("method", "name"),
        "property_declaration" if is_csharp => ("property", "name"),
        "method" if is_ruby => (if inside_type { "method" } else { "function" }, "name"),
        "singleton_method" | "alias" if is_ruby => ("method", "name"),
        "method_elem" if is_go => ("method", "name"),
        "annotation_type_element_declaration" if is_java => ("method", "name"),
        "method_definition" if is_ecmascript => ("method", "name"),
        "method_signature" | "abstract_method_signature" if is_typescript => ("method", "name"),
        "enumerator" | "enum_variant" => ("enum member", "name"),
        "enum_assignment" if is_typescript => ("enum member", "name"),
        "enum_constant" if is_java => ("enum member", "name"),
        "enum_member_declaration" if is_csharp => ("enum member", "name"),
        "property_identifier"
            if is_typescript
                && node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "enum_body") =>
        {
            ("enum member", "name")
        }
        "field_declaration" if !is_java && !is_go && !is_csharp => ("field", "declarator"),
        "field_identifier" | "type_identifier" if is_go && is_go_field_name(node) => {
            ("field", "name")
        }
        "identifier" if is_go && go_package_binding_kind(node).is_some() => (
            go_package_binding_kind(node).expect("binding kind checked above"),
            "name",
        ),
        "field_definition" if is_ecmascript => ("field", "property"),
        "public_field_definition" | "property_signature" if is_typescript => ("field", "name"),
        "variable_declarator" if is_ecmascript && is_callable_variable(node) => {
            ("function", "name")
        }
        "variable_declarator" if is_java && is_java_field_declarator(node) => ("field", "name"),
        "formal_parameter" if is_java && is_java_record_component(node) => ("field", "name"),
        _ => return None,
    })
}

fn is_java_field_declarator(node: Node<'_>) -> bool {
    node.parent()
        .is_some_and(|parent| matches!(parent.kind(), "field_declaration" | "constant_declaration"))
}

fn is_java_record_component(node: Node<'_>) -> bool {
    node.parent()
        .and_then(|parent| parent.parent())
        .is_some_and(|grandparent| grandparent.kind() == "record_declaration")
}

pub(super) fn is_go_field_name(node: Node<'_>) -> bool {
    let Some(parent) = node
        .parent()
        .filter(|parent| parent.kind() == "field_declaration")
    else {
        return false;
    };
    node.kind() == "field_identifier"
        || (node.kind() == "type_identifier" && parent.child_by_field_name("name").is_none())
}

pub(super) fn go_package_binding_kind(node: Node<'_>) -> Option<&'static str> {
    let spec = node.parent()?;
    let kind = match spec.kind() {
        "const_spec" => "constant",
        "var_spec" => "variable",
        _ => return None,
    };
    let declaration = spec.parent()?;
    if !matches!(declaration.kind(), "const_declaration" | "var_declaration")
        || declaration.parent()?.kind() != "source_file"
    {
        return None;
    }
    let mut cursor = spec.walk();
    spec.children_by_field_name("name", &mut cursor)
        .any(|name| name == node)
        .then_some(kind)
}

pub(super) fn is_callable_variable(node: Node<'_>) -> bool {
    node.child_by_field_name("value").is_some_and(|value| {
        matches!(
            value.kind(),
            "arrow_function" | "function_expression" | "generator_function"
        )
    }) && node
        .child_by_field_name("name")
        .is_some_and(|name| name.kind() == "identifier")
}

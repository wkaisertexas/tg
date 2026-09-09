use std::path::Path;
use tree_sitter::Language;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Flavor {
    Bash,
    C,
    CSharp,
    Cpp,
    Go,
    Java,
    JavaScript,
    Rust,
    Python,
    Ruby,
    TypeScript,
    Tsx,
}

fn grammar(path: &Path) -> Option<(Language, Flavor)> {
    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        if matches!(
            name,
            ".bashrc"
                | ".bash_profile"
                | ".bash_login"
                | ".bash_logout"
                | ".profile"
                | "bash.bashrc"
                | "profile"
                | "PKGBUILD"
                | "APKBUILD"
        ) {
            return Some((tree_sitter_bash::LANGUAGE.into(), Flavor::Bash));
        }
        if name == "Jakefile" {
            return Some((tree_sitter_javascript::LANGUAGE.into(), Flavor::JavaScript));
        }
        if matches!(
            name,
            "Gemfile"
                | "Rakefile"
                | "Guardfile"
                | "Vagrantfile"
                | "Podfile"
                | "Fastfile"
                | "Appfile"
                | "Dangerfile"
                | "Berksfile"
                | "Capfile"
        ) {
            return Some((tree_sitter_ruby::LANGUAGE.into(), Flavor::Ruby));
        }
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "sh" | "bash" => Some((tree_sitter_bash::LANGUAGE.into(), Flavor::Bash)),
        "c" => Some((tree_sitter_c::LANGUAGE.into(), Flavor::C)),
        "cs" => Some((tree_sitter_c_sharp::LANGUAGE.into(), Flavor::CSharp)),
        "h" | "hh" | "hpp" | "hxx" | "cc" | "cpp" | "cxx" => {
            Some((tree_sitter_cpp::LANGUAGE.into(), Flavor::Cpp))
        }
        "rs" => Some((tree_sitter_rust::LANGUAGE.into(), Flavor::Rust)),
        "rb" | "gemspec" | "rake" | "ru" => Some((tree_sitter_ruby::LANGUAGE.into(), Flavor::Ruby)),
        "py" | "pyi" => Some((tree_sitter_python::LANGUAGE.into(), Flavor::Python)),
        "go" => Some((tree_sitter_go::LANGUAGE.into(), Flavor::Go)),
        "java" => Some((tree_sitter_java::LANGUAGE.into(), Flavor::Java)),
        "js" | "mjs" | "cjs" | "jsx" => {
            Some((tree_sitter_javascript::LANGUAGE.into(), Flavor::JavaScript))
        }
        "ts" | "mts" | "cts" => Some((
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Flavor::TypeScript,
        )),
        "tsx" => Some((tree_sitter_typescript::LANGUAGE_TSX.into(), Flavor::Tsx)),
        _ => None,
    }
}

pub(super) fn grammar_for_source(path: &Path, source: &str) -> Option<(Language, Flavor)> {
    grammar(path).or_else(|| {
        (path.extension().is_none() && has_shell_shebang(source))
            .then(|| (tree_sitter_bash::LANGUAGE.into(), Flavor::Bash))
    })
}

fn has_shell_shebang(source: &str) -> bool {
    let Some(line) = source
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("#!"))
    else {
        return false;
    };
    let mut words = line.split_ascii_whitespace();
    let Some(interpreter) = words.next() else {
        return false;
    };
    let command = if executable_name(interpreter) == Some("env") {
        match words.next() {
            Some("-S") => words.next(),
            Some(word) if !word.starts_with('-') => Some(word),
            _ => None,
        }
    } else {
        Some(interpreter)
    };
    command
        .and_then(executable_name)
        .is_some_and(|name| matches!(name, "sh" | "bash" | "dash" | "ash"))
}

fn executable_name(command: &str) -> Option<&str> {
    Path::new(command).file_name()?.to_str()
}

pub fn supports(path: &Path) -> bool {
    grammar(path).is_some() || super::markdown::is_markdown(path)
}

/// Returns whether source contents may identify an otherwise unsupported path.
/// This path-only prefilter keeps filesystem reads in the background indexer.
pub(crate) fn may_support_with_source(path: &Path) -> bool {
    supports(path)
        || (path.extension().is_none()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.starts_with('.')))
}

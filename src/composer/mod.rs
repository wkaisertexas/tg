use crate::{language, search};
use anyhow::{Context, Result};
use std::path::Path;

pub fn resolve_prompt(search_root: &Path, prompt: &str) -> Result<String> {
    let mut output = String::with_capacity(prompt.len());
    let bytes = prompt.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let sigil = bytes[index];
        let boundary = index == 0
            || bytes[index - 1].is_ascii_whitespace()
            || b"([{<\"'".contains(&bytes[index - 1]);
        if (sigil == b'@' || sigil == b'%') && boundary {
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() {
                let symbol_stage = prompt[start..end].contains("::");
                if bytes[end].is_ascii_whitespace()
                    || b",;!?)]}>\"'".contains(&bytes[end])
                    || (symbol_stage && bytes[end] == b'.')
                {
                    break;
                }
                end += 1;
            }
            let mut token = &prompt[start..end];
            if !token.contains("::") && !search_root.join(token).is_file() && token.ends_with('.') {
                end -= 1;
                token = &prompt[start..end];
            }
            if !token.is_empty() {
                let (file, symbol) = token
                    .split_once("::")
                    .map_or((token, None), |(f, s)| (f, Some(s)));
                let path = search::resolve_exact(search_root, file)
                    .with_context(|| format!("unresolved file reference {file}"))?;
                output.push_str(file);
                if let Some(query) = symbol {
                    let parsed = language::parse(&path)?
                        .context("symbol completion is unavailable for this file")?;
                    let selected = language::resolve_unique(&parsed.symbols, query)?;
                    output.push_str(&format!(
                        ":{}:{}",
                        selected.start.line, selected.start.column
                    ));
                }
                index = end;
                continue;
            }
        }
        let character = prompt[index..].chars().next().unwrap();
        output.push(character);
        index += character.len_utf8();
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn leaves_plain_text_untouched() {
        assert_eq!(resolve_prompt(Path::new("."), "hello").unwrap(), "hello");
    }
}

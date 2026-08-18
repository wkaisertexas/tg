use base64::{Engine, engine::general_purpose::STANDARD as BASE64};

/// Encode text for the terminal clipboard without writing it to the terminal.
pub fn osc52_sequence(text: &str) -> String {
    format!("\u{1b}]52;c;{}\u{7}", BASE64.encode(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_preserves_multiline_unicode_bytes() {
        assert_eq!(
            osc52_sequence("first\nλ\n"),
            "\u{1b}]52;c;Zmlyc3QKzrsK\u{7}"
        );
    }
}

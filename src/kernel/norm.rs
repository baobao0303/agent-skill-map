//! Trigger normalization (spec_doc §10): NFD → drop Mn → lowercase →
//! `-`/`_`/whitespace runs to one space → trim.

use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

pub fn normalize_trigger(input: &str) -> String {
    let stripped: String = input
        .nfd()
        .filter(|&c| get_general_category(c) != GeneralCategory::NonspacingMark)
        .collect();
    let mut out = String::with_capacity(stripped.len());
    let mut pending_space = false;
    for c in stripped.to_lowercase().chars() {
        if c == '-' || c == '_' || is_js_space(c) {
            pending_space = true;
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;
        out.push(c);
    }
    out
}

/// JavaScript's `\s` class.
pub fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::normalize_trigger;

    #[test]
    fn spec_examples() {
        for (input, want) in [
            ("Hacer Review", "hacer review"),
            ("hacer-review", "hacer review"),
            ("hacer_review", "hacer review"),
            ("  hacer   review  ", "hacer review"),
            ("Clúster", "cluster"),
            ("/MyCommand", "/mycommand"),
            ("@FooExtractor", "@fooextractor"),
            ("skill-map:explore", "skill map:explore"),
            ("a\u{00A0}b", "a b"),
        ] {
            assert_eq!(normalize_trigger(input), want, "{input:?}");
        }
    }
}

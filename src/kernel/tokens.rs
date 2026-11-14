//! Token counts with `cl100k_base`. skill-map counts with gpt-tokenizer and an
//! empty `disallowedSpecial` set, i.e. special-token text is plain text —
//! the same as tiktoken's `encode_ordinary`.

use std::sync::LazyLock;
use tiktoken_rs::CoreBPE;

static CL100K: LazyLock<CoreBPE> = LazyLock::new(|| tiktoken_rs::cl100k_base().expect("cl100k_base"));

pub fn count(text: &str) -> u64 {
    if text.is_empty() { 0 } else { CL100K.encode_ordinary(text).len() as u64 }
}

#[cfg(test)]
mod tests {
    /// Token triples must match the original CLI on the example fixture.
    #[test]
    fn matches_oracle_example() {
        let base = concat!(env!("CARGO_MANIFEST_DIR"), "/conformance/");
        let scan: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(format!("{base}golden/example/scan.json")).unwrap()).unwrap();
        for node in scan["nodes"].as_array().unwrap() {
            let Some(tokens) = node.get("tokens") else { continue };
            let path = node["path"].as_str().unwrap();
            let text = std::fs::read_to_string(format!("{base}fixtures/example/{path}")).unwrap();
            let p = crate::frontmatter::parse(&text);
            assert_eq!(super::count(p.raw), tokens["frontmatter"], "{path} frontmatter");
            assert_eq!(super::count(p.body), tokens["body"], "{path} body");
        }
    }
}

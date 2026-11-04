//! Pure helpers for reading fields of feed records, available without any feature so IO-free domain
//! crates of an app can use them (the `replica` mappings use the same ones).

use serde_json::Value;

/// Longest kept text (characters); longer strings are cut.
pub const TEXT_MAX: usize = 200;

/// The string field `name` of the object `v`, trimmed and cut to [`TEXT_MAX`] characters; `None`
/// when missing, not a string or blank.
pub fn text_field(v: &Value, name: &str) -> Option<String> {
    let s = v.get(name)?.as_str()?.trim();
    (!s.is_empty()).then(|| s.chars().take(TEXT_MAX).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_fields() {
        let v = json!({ "a": "  x ", "b": "  ", "c": 3, "long": "ä".repeat(300) });
        assert_eq!(text_field(&v, "a").as_deref(), Some("x"));
        assert_eq!(text_field(&v, "b"), None);
        assert_eq!(text_field(&v, "c"), None);
        assert_eq!(text_field(&v, "missing"), None);
        assert_eq!(
            text_field(&v, "long").map(|s| s.chars().count()),
            Some(TEXT_MAX)
        );
    }
}

//! Reading a GraphQL answer: `data` and a bounded `errors` list.

use super::error::ResponseError;
use serde::Deserialize;
use serde_json::Value;

/// Most errors of one answer that are kept; the rest is dropped.
pub const MAX_ERRORS: usize = 20;
/// Longest kept `message` in characters; longer ones are cut.
pub const MAX_MESSAGE_CHARS: usize = 512;
/// Longest kept `extensions.code` in characters.
const MAX_CODE_CHARS: usize = 128;
/// Most kept entries of `extensions.fields`; the rest is dropped.
pub const MAX_FIELDS: usize = 50;
/// Longest kept field path of `extensions.fields` in characters.
pub const MAX_FIELD_PATH_CHARS: usize = 128;

/// The envelope of an answer; everything else in it is ignored.
#[derive(Deserialize)]
struct Envelope {
    data: Option<Value>,
    errors: Option<Vec<RawError>>,
}

/// One entry of `errors`, leniently: any field may be missing or of another type.
#[derive(Deserialize)]
struct RawError {
    message: Option<Value>,
    extensions: Option<Value>,
}

/// An answer split into its parts; `data` is `None` for a missing or `null` `data`.
#[derive(Debug)]
pub(crate) struct Parsed {
    pub(crate) data: Option<Value>,
    pub(crate) errors: Vec<ResponseError>,
}

/// Parses `body` as GraphQL answer; `Err` if it is not a JSON object of the expected shape.
pub(crate) fn parse(body: &[u8]) -> Result<Parsed, ()> {
    let env: Envelope = serde_json::from_slice(body).map_err(|_| ())?;
    let errors = env
        .errors
        .unwrap_or_default()
        .into_iter()
        .take(MAX_ERRORS)
        .map(into_error)
        .collect();
    Ok(Parsed {
        data: env.data.filter(|d| !d.is_null()),
        errors,
    })
}

/// `s` without control characters (so a server cannot inject line breaks or escape codes into logs
/// or terminals), cut to `max` characters.
fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// A bounded [`ResponseError`] from the raw entry.
fn into_error(raw: RawError) -> ResponseError {
    let message = match raw.message {
        Some(Value::String(s)) => clean(&s, MAX_MESSAGE_CHARS),
        _ => String::new(),
    };
    let code = match raw.extensions.as_ref().and_then(|e| e.get("code")) {
        Some(Value::String(s)) => Some(clean(s, MAX_CODE_CHARS)),
        _ => None,
    };
    let fields = fields(raw.extensions.as_ref().and_then(|e| e.get("fields")));
    ResponseError {
        message,
        code,
        fields,
    }
}

/// The bounded string entries of an `extensions.fields` object, sorted by path, with unique non-empty
/// paths; empty for anything else.
fn fields(value: Option<&Value>) -> Vec<(String, String)> {
    let Some(Value::Object(map)) = value else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = map
        .iter()
        .filter_map(|(path, msg)| match msg {
            Value::String(m) => Some((
                clean(path, MAX_FIELD_PATH_CHARS),
                clean(m, MAX_MESSAGE_CHARS),
            )),
            _ => None,
        })
        .collect();
    // Sorted explicitly: the map order depends on serde_json's `preserve_order` feature.
    out.sort();
    // Cleaning can make paths empty or equal ("a" and "a\u{7}"); keep the first of each, drop empty ones.
    out.retain(|(path, _)| !path.is_empty());
    out.dedup_by(|b, a| a.0 == b.0);
    out.truncate(MAX_FIELDS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_only() {
        let p = parse(br#"{"data":{"a":1}}"#).unwrap();
        assert_eq!(p.data, Some(serde_json::json!({"a": 1})));
        assert!(p.errors.is_empty());
    }

    #[test]
    fn null_data_and_errors_with_codes() {
        let p = parse(
            br#"{"data":null,"errors":[{"message":"no","extensions":{"code":"VALIDATION"}},
                {"message":"x","extensions":{"code":7}},{"extensions":"odd"},{"message":5}]}"#,
        )
        .unwrap();
        assert!(p.data.is_none());
        assert_eq!(p.errors.len(), 4);
        assert_eq!(p.errors[0].code.as_deref(), Some("VALIDATION"));
        assert_eq!(p.errors[0].message, "no");
        assert_eq!(p.errors[1].code, None);
        assert_eq!(p.errors[2].message, "");
        assert_eq!(p.errors[3].message, "");
    }

    #[test]
    fn errors_and_messages_are_bounded() {
        let long = "ä".repeat(MAX_MESSAGE_CHARS + 10);
        let one = format!(r#"{{"message":"{long}"}}"#);
        let many = vec![one; MAX_ERRORS + 5].join(",");
        let p = parse(format!(r#"{{"errors":[{many}]}}"#).as_bytes()).unwrap();
        assert_eq!(p.errors.len(), MAX_ERRORS);
        assert_eq!(p.errors[0].message.chars().count(), MAX_MESSAGE_CHARS);
    }

    #[test]
    fn control_characters_are_stripped() {
        let p = parse(
            br#"{"errors":[{"message":"a\u001b[31mb\nc\u0000","extensions":{"code":"X\tY"}}]}"#,
        )
        .unwrap();
        assert_eq!(p.errors[0].message, "a[31mbc");
        assert_eq!(p.errors[0].code.as_deref(), Some("XY"));
    }

    #[test]
    fn fields_are_kept_sorted_cleaned_and_bounded() {
        let p = parse(
            br#"{"errors":[{"message":"m","extensions":{"code":"VALIDATION",
                "fields":{"z":"Zz","a.0":"A\nb","n":5}}},
                {"extensions":{"fields":"odd"}},{"extensions":{"fields":["x"]}}]}"#,
        )
        .unwrap();
        assert_eq!(
            p.errors[0].fields,
            vec![("a.0".into(), "Ab".into()), ("z".into(), "Zz".into())]
        );
        assert!(p.errors[1].fields.is_empty() && p.errors[2].fields.is_empty());

        let many: Vec<String> = (0..MAX_FIELDS + 5)
            .map(|i| format!(r#""f{i:03}":"m""#))
            .collect();
        let long = "p".repeat(MAX_FIELD_PATH_CHARS + 10);
        let body = format!(
            r#"{{"errors":[{{"extensions":{{"fields":{{{}}}}}}},{{"extensions":{{"fields":{{"{long}":"m"}}}}}}]}}"#,
            many.join(",")
        );
        let p = parse(body.as_bytes()).unwrap();
        assert_eq!(p.errors[0].fields.len(), MAX_FIELDS);
        assert_eq!(p.errors[0].fields[0].0, "f000");
        assert_eq!(
            p.errors[0].fields[MAX_FIELDS - 1].0,
            format!("f{:03}", MAX_FIELDS - 1)
        );
        assert_eq!(
            p.errors[1].fields[0].0.chars().count(),
            MAX_FIELD_PATH_CHARS
        );
    }

    #[test]
    fn field_paths_are_cleaned_unique_and_messages_cut() {
        let long = "ü".repeat(MAX_MESSAGE_CHARS + 3);
        let body = format!(
            r#"{{"errors":[{{"extensions":{{"fields":{{"a":"1","a\u0007":"2","\n":"3","b\tc":"{long}"}}}}}}]}}"#
        );
        let p = parse(body.as_bytes()).unwrap();
        let f = &p.errors[0].fields;
        assert_eq!(f.len(), 2);
        assert_eq!(f[0], ("a".into(), "1".into()));
        assert_eq!(f[1].0, "bc");
        assert_eq!(f[1].1.chars().count(), MAX_MESSAGE_CHARS);
    }

    #[test]
    fn not_an_answer() {
        for bad in [&b"[]"[..], b"nope", b"", br#"{"errors":"x"}"#, b"null"] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}

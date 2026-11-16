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

/// A bounded [`ResponseError`] from the raw entry.
fn into_error(raw: RawError) -> ResponseError {
    let message = match raw.message {
        Some(Value::String(s)) => s.chars().take(MAX_MESSAGE_CHARS).collect(),
        _ => String::new(),
    };
    let code = match raw.extensions.as_ref().and_then(|e| e.get("code")) {
        Some(Value::String(s)) => Some(s.chars().take(MAX_CODE_CHARS).collect()),
        _ => None,
    };
    ResponseError { message, code }
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
    fn not_an_answer() {
        for bad in [&b"[]"[..], b"nope", b"", br#"{"errors":"x"}"#, b"null"] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}

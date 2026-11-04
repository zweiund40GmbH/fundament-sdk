//! The query of a feed request (`since`, `limit`, `types`, `docs/protocol.md` §2) and the rule for
//! record type names. Pure functions without IO; the endpoint answers a [`QueryError`] with `400`.

use crate::protocol::MAX_PAGE_LIMIT;

/// Page size when `limit` is missing.
pub const DEFAULT_LIMIT: u32 = 100;
/// Longest record type URN.
pub const TYPE_MAX: usize = 128;

/// A valid feed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedQuery {
    /// Events with a cursor above this one (`0` = from the oldest).
    pub since: u64,
    /// Page size, 1–500.
    pub limit: u32,
    /// The types to read, each once: the requested ones, or every known type when `types` was missing
    /// or empty.
    pub types: Vec<String>,
}

/// Why a feed request is invalid. [`QueryError::message`] is the fixed German text for the caller
/// (`docs/protocol.md` §2.6); `Display` is English, for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QueryError {
    /// The query string cannot be decoded.
    #[error("query string cannot be decoded")]
    Malformed,
    /// A parameter occurs twice.
    #[error("parameter given twice")]
    Repeated,
    /// `since` is not a non-negative integer (or too large).
    #[error("invalid since")]
    Since,
    /// `limit` is not an integer in 1–500.
    #[error("invalid limit")]
    Limit,
    /// `types` names a type the source does not offer.
    #[error("unknown type")]
    UnknownType,
}

impl QueryError {
    /// The fixed German message of the `400` answer.
    pub fn message(self) -> &'static str {
        match self {
            QueryError::Malformed => "Ungültige Abfrageparameter.",
            QueryError::Repeated => "Jeder Parameter darf nur einmal vorkommen.",
            QueryError::Since => "„since“ muss eine ganze Zahl ab 0 sein.",
            QueryError::Limit => "„limit“ muss zwischen 1 und 500 liegen.",
            QueryError::UnknownType => "Unbekannter Typ in „types“.",
        }
    }
}

/// Whether `t` is a usable record type URN: starts with `urn:`, at most [`TYPE_MAX`] visible ASCII
/// characters, no comma (the separator of `types`).
pub fn is_valid_type(t: &str) -> bool {
    t.len() > "urn:".len()
        && t.len() <= TYPE_MAX
        && t.starts_with("urn:")
        && t.bytes().all(|b| b.is_ascii_graphic() && b != b',')
}

impl FeedQuery {
    /// Parses the query parameters (name/value pairs in order) against the types the source offers.
    /// Unknown parameters are ignored (additive evolution); a parameter given twice is an error.
    pub fn parse(params: &[(String, String)], known: &[String]) -> Result<Self, QueryError> {
        let (mut since, mut limit, mut types) = (None, None, None);
        for (k, v) in params {
            let slot = match k.as_str() {
                "since" => &mut since,
                "limit" => &mut limit,
                "types" => &mut types,
                _ => continue,
            };
            if slot.replace(v.as_str()).is_some() {
                return Err(QueryError::Repeated);
            }
        }
        Ok(Self {
            since: parse_since(since)?,
            limit: parse_limit(limit)?,
            types: parse_types(types, known)?,
        })
    }
}

/// `since`: missing or empty = 0, else digits only.
fn parse_since(raw: Option<&str>) -> Result<u64, QueryError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(0),
        // `parse` alone would accept a leading `+`.
        Some(s) if s.bytes().all(|b| b.is_ascii_digit()) => {
            s.parse().map_err(|_| QueryError::Since)
        }
        Some(_) => Err(QueryError::Since),
    }
}

/// `limit`: missing or empty = [`DEFAULT_LIMIT`], else 1–[`MAX_PAGE_LIMIT`].
fn parse_limit(raw: Option<&str>) -> Result<u32, QueryError> {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(DEFAULT_LIMIT);
    };
    s.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| s.parse::<u32>().ok())
        .flatten()
        .filter(|n| (1..=MAX_PAGE_LIMIT).contains(n))
        .ok_or(QueryError::Limit)
}

/// `types`: comma-separated URNs of `known`; missing or empty = all of `known`, unknown = error.
fn parse_types(raw: Option<&str>, known: &[String]) -> Result<Vec<String>, QueryError> {
    let mut out: Vec<String> = Vec::new();
    for t in raw.unwrap_or_default().split(',').map(str::trim) {
        if t.is_empty() {
            continue;
        }
        if !known.iter().any(|k| k == t) {
            return Err(QueryError::UnknownType);
        }
        if !out.iter().any(|o| o == t) {
            out.push(t.to_owned());
        }
    }
    if out.is_empty() {
        out = known.to_vec();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "urn:example:crm:customer";
    const B: &str = "urn:example:crm:site";

    fn q(pairs: &[(&str, &str)]) -> Result<FeedQuery, QueryError> {
        let v: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        FeedQuery::parse(&v, &[A.to_owned(), B.to_owned()])
    }

    #[test]
    fn defaults() {
        let d = q(&[("other", "1")]).unwrap();
        assert_eq!((d.since, d.limit), (0, DEFAULT_LIMIT));
        assert_eq!(d.types, [A, B], "no types = every known type");
        let blank = q(&[("since", ""), ("limit", " "), ("types", ",")]).unwrap();
        assert_eq!((blank.since, blank.limit, blank.types.len()), (0, 100, 2));
    }

    #[test]
    fn since_and_limit() {
        let a = q(&[("since", "42"), ("limit", "500")]).unwrap();
        assert_eq!((a.since, a.limit), (42, 500));
        assert_eq!(q(&[("limit", "1")]).unwrap().limit, 1);
        for bad in ["0", "501", "-1", "x", "1.5", "+5"] {
            assert_eq!(q(&[("limit", bad)]), Err(QueryError::Limit), "{bad}");
        }
        for bad in ["-1", "+3", "a", "99999999999999999999999"] {
            assert_eq!(q(&[("since", bad)]), Err(QueryError::Since), "{bad}");
        }
        assert_eq!(
            q(&[("since", "1"), ("since", "2")]),
            Err(QueryError::Repeated)
        );
    }

    #[test]
    fn types_are_known_urns() {
        let a = q(&[("types", &format!("{B}, {A},{B}"))]).unwrap();
        assert_eq!(a.types, [B, A], "requested order, each once");
        assert_eq!(q(&[("types", "customer")]), Err(QueryError::UnknownType));
        assert_eq!(
            q(&[("types", &format!("{A},urn:example:x:y"))]),
            Err(QueryError::UnknownType)
        );
    }

    #[test]
    fn messages_are_fixed_german_texts() {
        assert_eq!(
            QueryError::Limit.message(),
            "„limit“ muss zwischen 1 und 500 liegen."
        );
        assert_eq!(QueryError::Limit.to_string(), "invalid limit");
    }

    #[test]
    fn type_names() {
        assert!(is_valid_type(A));
        for bad in ["", "urn:", "customer", "urn:a,b", "urn:a b", "urn:ä"] {
            assert!(!is_valid_type(bad), "{bad}");
        }
        assert!(!is_valid_type(&format!("urn:{}", "x".repeat(TYPE_MAX))));
    }
}

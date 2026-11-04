//! [`KeepFields`]: a mapping that keeps only the named fields of each record type, so nothing else of
//! the feed (contact data, notes …) reaches the app's database. Plus [`text_field`] for hand-written
//! mappings.

use super::{InvalidKind, Mapping, Projection, check_kind};
use serde_json::{Map, Value};

/// Longest kept text (characters); longer strings are cut.
pub const TEXT_MAX: usize = 200;

/// The string field `name` of the object `v`, trimmed and cut to [`TEXT_MAX`] characters; `None`
/// when missing, not a string or blank.
pub fn text_field(v: &Value, name: &str) -> Option<String> {
    let s = v.get(name)?.as_str()?.trim();
    (!s.is_empty()).then(|| s.chars().take(TEXT_MAX).collect())
}

/// One kept type.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Kept {
    event_type: String,
    kind: String,
    fields: Vec<String>,
}

/// Keeps named fields per record type. Paths use dots for nested objects (`address.city`). Strings
/// are trimmed and cut to [`TEXT_MAX`] characters, blank strings and missing fields are left out;
/// other values are kept as they are. Types that are not listed are skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeepFields(Vec<Kept>);

impl KeepFields {
    /// A mapping that keeps nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Keeps `fields` of events of `event_type`, stored under `kind` (1 to 32 characters).
    pub fn keep(
        mut self,
        event_type: &str,
        kind: &str,
        fields: &[&str],
    ) -> Result<Self, InvalidKind> {
        check_kind(kind)?;
        self.0.push(Kept {
            event_type: event_type.to_owned(),
            kind: kind.to_owned(),
            fields: fields.iter().map(|f| (*f).to_owned()).collect(),
        });
        Ok(self)
    }
}

/// The value at the dotted `path` of `v`.
fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |v, part| v.get(part))
}

/// Cleans a kept value: strings trimmed and cut, blank strings dropped.
fn clean(v: &Value) -> Option<Value> {
    match v {
        Value::Null => None,
        Value::String(s) => {
            let s = s.trim();
            (!s.is_empty()).then(|| Value::String(s.chars().take(TEXT_MAX).collect()))
        }
        other => Some(other.clone()),
    }
}

/// Inserts `value` at the dotted `path` of `out`, creating objects on the way.
fn insert(out: &mut Map<String, Value>, path: &str, value: Value) {
    let mut parts = path.split('.').peekable();
    let mut obj = out;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            obj.insert(part.to_owned(), value);
            return;
        }
        let next = obj
            .entry(part.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        let Value::Object(inner) = next else { return };
        obj = inner;
    }
}

impl Mapping for KeepFields {
    fn kind(&self, event_type: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|k| k.event_type == event_type)
            .map(|k| k.kind.as_str())
    }

    fn project(&self, event_type: &str, data: &Value) -> Projection {
        let Some(kept) = self.0.iter().find(|k| k.event_type == event_type) else {
            return Projection::default();
        };
        let mut out = Map::new();
        for path in &kept.fields {
            if let Some(v) = at(data, path).and_then(clean) {
                insert(&mut out, path, v);
            }
        }
        Projection {
            data: (!out.is_empty()).then_some(Value::Object(out)),
            external_id: None,
        }
    }
}

//! The status document with the wire names of version 1 (`docs/protocol.md` §3), and reading it
//! ([`StatusDocument::from_json`]). Pure types without IO, always compiled; the `status` and
//! `status-client` features re-export them under [`crate::status`]. The Rust names are English; the
//! JSON names (some German) are part of the wire format and fixed by `serde` renames.
//!
//! Reading is lenient (`docs/protocol.md` §3.3): unknown fields are ignored, missing lists are empty,
//! a missing `version` or `stand` is an empty string and missing `null`able fields are `None`; `null`
//! counts as missing for the lists, `version` and `stand`. The
//! figures (`letzterCursor`, `ereignisseProMinute`, `ereignisse24h`, `rueckstand`) are required, so `0`
//! always means 0. The `serde(default)`s only affect reading; a built document writes every field.

mod parse;

pub use parse::InvalidDocument;

use crate::protocol::Level;
use serde::{Deserialize, Deserializer, Serialize};

/// Reads `null` like a missing value: the default (lenient fields only; serializing is unaffected).
fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// The whole document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusDocument {
    /// Name of the app (`app`).
    pub app: String,
    /// Version of the app (`version`).
    #[serde(default, deserialize_with = "null_as_default")]
    pub version: String,
    /// Time the document was built (`stand`, RFC 3339 UTC).
    #[serde(rename = "stand", default, deserialize_with = "null_as_default")]
    pub as_of: String,
    /// Overall level (`zustand`).
    #[serde(rename = "zustand")]
    pub state: Level,
    /// Notes (`hinweise`).
    #[serde(rename = "hinweise", default, deserialize_with = "null_as_default")]
    pub notes: Vec<Note>,
    /// What the app offers (`angebote`).
    #[serde(rename = "angebote", default, deserialize_with = "null_as_default")]
    pub offers: Vec<Offer>,
    /// Who polls the app (`abrufer`).
    #[serde(rename = "abrufer", default, deserialize_with = "null_as_default")]
    pub pollers: Vec<Poller>,
    /// Sources the app reads from (`bezieht`).
    #[serde(rename = "bezieht", default, deserialize_with = "null_as_default")]
    pub sources: Vec<Source>,
    /// The newest events of the app, newest first (`ereignisse`).
    #[serde(rename = "ereignisse", default, deserialize_with = "null_as_default")]
    pub events: Vec<StatusEvent>,
}

/// A note (`hinweise[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Level (`stufe`).
    #[serde(rename = "stufe")]
    pub level: Level,
    /// Fixed text for humans (`text`).
    pub text: String,
}

/// An offer, e.g. one feed type (`angebote[]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Offer {
    /// Kind, e.g. `feed` (`art`).
    #[serde(rename = "art")]
    pub kind: String,
    /// Record type URN (`typ`).
    #[serde(rename = "typ")]
    pub record_type: String,
    /// Newest cursor of the type (`letzterCursor`).
    #[serde(rename = "letzterCursor")]
    pub last_cursor: u64,
    /// Events per minute, one decimal (`ereignisseProMinute`).
    #[serde(rename = "ereignisseProMinute")]
    pub events_per_minute: f64,
    /// Events in the last 24 hours (`ereignisse24h`).
    #[serde(rename = "ereignisse24h")]
    pub events_24h: u64,
}

/// A client polling the app, by the name of its credential (`abrufer[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Poller {
    /// Name of the credential (`name`).
    pub name: String,
    /// Kind, e.g. `feed` (`art`).
    #[serde(rename = "art")]
    pub kind: String,
    /// Last poll (`letzterAbruf`).
    #[serde(rename = "letzterAbruf")]
    pub last_poll: Option<String>,
    /// Cursor of that poll (`cursor`).
    pub cursor: Option<u64>,
    /// Events after `cursor`, capped at a source-defined maximum (`rueckstand`; `BACKLOG_CAP` of the
    /// `status` feature).
    #[serde(rename = "rueckstand")]
    pub backlog: u64,
    /// Failed attempts of this client in 24 hours; `None` when they cannot be attributed
    /// (`fehlversuche24h`).
    #[serde(rename = "fehlversuche24h")]
    pub failures_24h: Option<u64>,
}

/// A source the app reads from (`bezieht[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// Name of the source (`quelle`).
    #[serde(rename = "quelle")]
    pub source: String,
    /// Kind, e.g. `feed` (`art`).
    #[serde(rename = "art")]
    pub kind: String,
    /// End of the last run (`letzterAbruf`).
    #[serde(rename = "letzterAbruf")]
    pub last_poll: Option<String>,
    /// Level (`zustand`).
    #[serde(rename = "zustand")]
    pub state: Level,
    /// Fixed error text (`text`).
    pub text: Option<String>,
}

/// An event of the app (`ereignisse[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusEvent {
    /// Time (`zeit`, RFC 3339 UTC).
    #[serde(rename = "zeit")]
    pub time: String,
    /// Kind, free text (`art`).
    #[serde(rename = "art")]
    pub kind: String,
    /// Level (`stufe`).
    #[serde(rename = "stufe")]
    pub level: Level,
    /// Fixed text without personal data (`text`).
    pub text: String,
}

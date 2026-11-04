//! Static service keys between apps (until token credentials replace them): verifying incoming bearer
//! keys, braking guessers, and sending an outgoing key.
//!
//! * [`KeyList`] – configured `name=sha256hex` entries; only the hash of a key is configured, never the key
//! * [`FailureBrake`] – failed attempts per client address and minute
//! * [`BearerKey`] – the key this app sends to another app
//! * [`bearer_token`] – the key of an `Authorization: Bearer <key>` header value

mod bearer;
mod brake;
mod keys;

pub use bearer::{BearerKey, InvalidKey, bearer_token};
pub use brake::{FailureBrake, MAX_FAILURES, WINDOW, brake_address, shortened_for_logs};
pub use keys::{KeyList, KeyListError, key_entry, name_is_valid};

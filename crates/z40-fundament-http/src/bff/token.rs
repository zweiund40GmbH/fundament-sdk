//! Random cookie values and what is derived from them. A session or login cookie carries 256 random bits
//! (base64url, 43 characters); the server keeps only their SHA-256 ([`TokenHash`]), so a copy of the
//! database cannot be turned into a valid cookie.
//!
//! Values that must be kept with a session or a started login but are only needed while the browser
//! presents the cookie (the login state with the PKCE verifier, the ID token for the logout hint) are
//! sealed with AES-256-GCM under a key derived from the cookie value. The key is
//! never stored: the hash in the database is SHA-256 of the bare value, the key SHA-256 of a label plus
//! the value, and neither reveals the other.

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use aws_lc_rs::digest::{Context, SHA256, digest};
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Length of a cookie value in characters (32 random bytes, base64url without padding).
pub const TOKEN_LEN: usize = 43;

/// Random bytes per cookie value (256 bit).
const TOKEN_BYTES: usize = 32;

/// The operating system could not provide randomness, or sealing failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("cryptographic operation failed")]
pub struct CryptoError;

/// SHA-256 of a cookie value: the only form of a session or login id that is stored. `Debug` prints no
/// bytes, so log lines cannot be used to correlate sessions.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenHash([u8; 32]);

impl TokenHash {
    /// The hash of the cookie value `token`.
    pub fn of(token: &str) -> Self {
        let mut out = [0u8; 32];
        out.copy_from_slice(digest(&SHA256, token.as_bytes()).as_ref());
        Self(out)
    }

    /// A hash read back from storage.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The 32 bytes (e.g. for a `BINARY(32)` column).
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for TokenHash {
    /// Prints a placeholder.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenHash(..)")
    }
}

/// A new random cookie value (32 bytes from the operating system, base64url).
pub(crate) fn generate() -> Result<String, CryptoError> {
    let mut buf = [0u8; TOKEN_BYTES];
    SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| CryptoError)?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

/// Whether `s` has the form of a cookie value; garbage is rejected before any storage lookup.
pub fn is_well_formed(s: &str) -> bool {
    s.len() == TOKEN_LEN
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// What a sealed value is for; part of the key derivation, so a value sealed for one purpose never
/// opens for another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Purpose {
    /// The serialized login state of a started login.
    LoginState,
    /// The ID token of a session (logout hint).
    IdToken,
}

impl Purpose {
    /// The label hashed in front of the cookie value.
    fn label(self) -> &'static [u8] {
        match self {
            Self::LoginState => b"z40-fundament-http/bff/login-state/v1\0",
            Self::IdToken => b"z40-fundament-http/bff/id-token/v1\0",
        }
    }
}

/// The AES-256-GCM key of `purpose` for the cookie value `token`.
fn key(token: &str, purpose: Purpose) -> Result<LessSafeKey, CryptoError> {
    let mut ctx = Context::new(&SHA256);
    ctx.update(purpose.label());
    ctx.update(token.as_bytes());
    let unbound = UnboundKey::new(&AES_256_GCM, ctx.finish().as_ref()).map_err(|_| CryptoError)?;
    Ok(LessSafeKey::new(unbound))
}

/// Encrypts `plain` for `purpose` under a key derived from `token`: random nonce, then ciphertext and tag.
pub(crate) fn seal(token: &str, purpose: Purpose, plain: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| CryptoError)?;
    let mut buf = plain.to_vec();
    key(token, purpose)?
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buf)
        .map_err(|_| CryptoError)?;
    let mut out = nonce.to_vec();
    out.append(&mut buf);
    Ok(out)
}

/// Decrypts what [`seal`] produced; `None` for a wrong token, purpose or a modified value.
pub(crate) fn open(token: &str, purpose: Purpose, sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < NONCE_LEN {
        return None;
    }
    let (nonce, cipher) = sealed.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
    let mut buf = cipher.to_vec();
    let plain = key(token, purpose)
        .ok()?
        .open_in_place(nonce, Aad::empty(), &mut buf)
        .ok()?;
    Some(plain.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn values_are_random_well_formed_and_carry_256_bits() {
        let values: HashSet<String> = (0..1000).map(|_| generate().unwrap()).collect();
        assert_eq!(values.len(), 1000, "no repeats");
        for v in &values {
            assert!(is_well_formed(v), "{v}");
            let bytes = URL_SAFE_NO_PAD.decode(v).unwrap();
            assert_eq!(bytes.len(), 32);
        }
        // Every bit position takes both values across the sample (no stuck bits).
        let decoded: Vec<Vec<u8>> = values
            .iter()
            .map(|v| URL_SAFE_NO_PAD.decode(v).unwrap())
            .collect();
        for bit in 0..256 {
            let ones = decoded
                .iter()
                .filter(|b| b[bit / 8] >> (bit % 8) & 1 == 1)
                .count();
            assert!((300..=700).contains(&ones), "bit {bit}: {ones} of 1000");
        }
    }

    #[test]
    fn malformed_values_are_refused() {
        let ok = "a".repeat(TOKEN_LEN);
        assert!(is_well_formed(&ok));
        for bad in [
            String::new(),
            "a".repeat(TOKEN_LEN - 1),
            "a".repeat(TOKEN_LEN + 1),
            format!("{}+", "a".repeat(TOKEN_LEN - 1)),
            format!("{}=", "a".repeat(TOKEN_LEN - 1)),
            format!("{}ä", "a".repeat(TOKEN_LEN - 2)),
            format!("{} ", "a".repeat(TOKEN_LEN - 1)),
        ] {
            assert!(!is_well_formed(&bad), "{bad:?}");
        }
    }

    #[test]
    fn hash_is_sha256_of_the_value_and_hidden_in_debug() {
        // SHA-256("abc"), FIPS 180-2 test vector.
        let h = TokenHash::of("abc");
        assert_eq!(h.as_bytes()[..4], [0xba, 0x78, 0x16, 0xbf]);
        assert_eq!(TokenHash::from_bytes(*h.as_bytes()), h);
        assert_ne!(TokenHash::of("abd"), h);
        assert_eq!(format!("{h:?}"), "TokenHash(..)");
    }

    #[test]
    fn sealed_values_open_only_with_the_same_token_and_purpose() {
        let token = generate().unwrap();
        let sealed = seal(&token, Purpose::IdToken, b"eyJ.id.token").unwrap();
        assert!(!sealed.windows(5).any(|w| w == b"eyJ.i"), "not plain text");
        assert_eq!(
            open(&token, Purpose::IdToken, &sealed).unwrap(),
            b"eyJ.id.token"
        );
        assert!(open(&token, Purpose::LoginState, &sealed).is_none());
        assert!(open(&generate().unwrap(), Purpose::IdToken, &sealed).is_none());
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(open(&token, Purpose::IdToken, &tampered).is_none());
        assert!(open(&token, Purpose::IdToken, &sealed[..5]).is_none());
        let again = seal(&token, Purpose::IdToken, b"eyJ.id.token").unwrap();
        assert_ne!(again, sealed, "fresh nonce each time");
    }
}

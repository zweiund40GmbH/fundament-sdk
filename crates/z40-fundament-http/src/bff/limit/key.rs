//! [`AddressKey`]: the keyed hash that turns a client address into the key of its shared login limit
//! counter. A plain SHA-256 of an address can be reversed by hashing every IPv4 address (or every known
//! /64); an HMAC under a key only the instances of the app know cannot. The key is derived from the
//! BFF client secret, which all instances share anyway, so no new setting is needed.

use crate::bff::token::{CryptoError, TokenHash};
use aws_lc_rs::{hkdf, hmac};
use std::net::IpAddr;
use z40_fundament::SecretString;

/// HKDF `info` of the key; a new version of the key scheme gets a new label.
const INFO: &[u8] = b"z40-fundament-http/bff/login-limit/v1";

/// HMAC-SHA-256 key of the address counters, derived with HKDF-SHA-256 from the client secret. Never
/// logged or exposed: `Debug` prints a placeholder.
pub(crate) struct AddressKey(hmac::Key);

impl std::fmt::Debug for AddressKey {
    /// Prints a placeholder, never the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AddressKey(..)")
    }
}

impl AddressKey {
    /// The key for the client secret `secret`: equal on every instance with the same configuration.
    /// Fails only if the crypto library refuses the derivation.
    pub(crate) fn derive(secret: &SecretString) -> Result<Self, CryptoError> {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(secret.expose().as_bytes());
        let info = [INFO];
        let okm = prk
            .expand(&info, hmac::HMAC_SHA256)
            .map_err(|_| CryptoError)?;
        Ok(Self(hmac::Key::from(okm)))
    }

    /// The counter key of the client address `ip` (IPv6 already reduced to its /64): HMAC of `ip:<ip>`.
    pub(crate) fn of(&self, ip: IpAddr) -> TokenHash {
        let tag = hmac::sign(&self.0, format!("ip:{ip}").as_bytes());
        let mut out = [0u8; 32];
        out.copy_from_slice(tag.as_ref());
        TokenHash::from_bytes(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(secret: &str) -> AddressKey {
        AddressKey::derive(&SecretString::new(secret.to_owned())).unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn is_not_the_plain_hash_of_the_address() {
        let k = key("s3cr3t").of(ip("192.0.2.1"));
        assert_ne!(k, TokenHash::of("ip:192.0.2.1"));
    }

    #[test]
    fn same_secret_same_key_other_secret_other_key() {
        let a = key("s3cr3t").of(ip("192.0.2.1"));
        assert_eq!(a, key("s3cr3t").of(ip("192.0.2.1")), "every instance");
        assert_ne!(a, key("other").of(ip("192.0.2.1")));
        assert_ne!(a, key("s3cr3t").of(ip("192.0.2.2")));
    }

    #[test]
    fn debug_prints_no_key() {
        assert_eq!(format!("{:?}", key("s3cr3t")), "AddressKey(..)");
    }
}

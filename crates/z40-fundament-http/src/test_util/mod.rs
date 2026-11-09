//! Helpers for the tests of apps that use [`crate::bff`] (feature `test-util`). **Test-only:** enable
//! the feature in `[dev-dependencies]`, never in a production build.
//!
//! * [`FakeProvider`] – a login provider on `127.0.0.1` (plain HTTP): discovery, JWKS with one ES256 key,
//!   a token endpoint that checks the code, the PKCE verifier and the redirect URI, and back-channel
//!   logout tokens. The test plays the browser: it follows the app's redirect to the authorize URL
//!   with [`FakeProvider::authorize`] and brings the callback query back to the app.
//! * [`crate::bff::token::generate`] – a random cookie value as the crate makes them, e.g. to put a
//!   session into a store directly.
//!
//! The provider's issuer is a plain-HTTP loopback URL, so the app's test configuration needs
//! `allow_loopback_http` ([`crate::bff::Defaults`]).
//!
//! ```no_run
//! use z40_fundament_http::bff::{BffConfig, Defaults};
//! use z40_fundament_http::test_util::FakeProvider;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let provider = FakeProvider::start("crm").await?;
//! let issuer = provider.issuer().to_owned();
//! let env = move |k: &str| match k {
//!     "AUTH_ISSUER" => Some(issuer.clone()),
//!     "AUTH_CLIENT_ID" => Some("crm".to_owned()),
//!     "AUTH_CLIENT_SECRET" => Some("test".to_owned()),
//!     "AUTH_REDIRECT_URI" => Some("http://localhost/api/auth/callback".to_owned()),
//!     _ => None,
//! };
//! let dev = Defaults { allow_loopback_http: true, ..Defaults::default() };
//! let config = BffConfig::from_lookup_with("AUTH", env, dev)?;
//! // … build the app, GET /api/auth/login, then:
//! // let done = provider.authorize(&location, "user-1", Some("sid-1"), &["admin"], |_| {});
//! // GET /api/auth/callback?{done.query} with the login cookie.
//! # Ok(())
//! # }
//! ```

mod oidc;

pub use oidc::{Authorized, FakeProvider};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bff::token;

    #[test]
    fn generated_values_are_well_formed_and_distinct() {
        let (a, b) = (token::generate().unwrap(), token::generate().unwrap());
        assert!(token::is_well_formed(&a) && token::is_well_formed(&b));
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn provider_runs_on_loopback_and_signs() {
        let p = FakeProvider::start("billing").await.unwrap();
        assert!(p.issuer().starts_with("http://127.0.0.1:"));
        assert_eq!(p.client_id(), "billing");
        assert!(format!("{p:?}").contains("billing"));
        let token = p.logout_token(Some("u"), None);
        assert_eq!(token.split('.').count(), 3, "a JWT");
    }

    #[tokio::test]
    #[should_panic(expected = "not the authorize URL of this provider")]
    async fn authorize_refuses_a_foreign_url() {
        let p = FakeProvider::start("billing").await.unwrap();
        p.authorize(
            "https://login.example.com/authorize?x=1",
            "u",
            None,
            &[],
            |_| {},
        );
    }
}

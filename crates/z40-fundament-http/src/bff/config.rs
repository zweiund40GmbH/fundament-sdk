//! [`BffConfig`]: the client registration at the login provider and the session settings, read with
//! [`BffConfig::from_lookup`]. The app chooses the prefix, so existing variable names stay: with the
//! prefix `AUTH` the variables are `AUTH_ISSUER`, `AUTH_CLIENT_ID`, `AUTH_CLIENT_SECRET`, … .
//!
//! URLs must be `https://`; plain `http://` is accepted only for loopback hosts (`localhost`,
//! `127.0.0.1`, `[::1]`), i.e. local development. When the redirect URI is such a plain-HTTP URL, the
//! cookies drop `Secure` and the `__Host-` prefix ([`BffConfig::secure_cookies`]) – the only relaxed
//! setting, and only where the browser would otherwise refuse the cookie.

use super::cookies::SameSite;
use super::timeouts::Timeouts;
use url::{Host, Url};
use z40_fundament::SecretString;
use z40_fundament::config::{ConfigError, ConfigVar, Lookup};

/// Settings of the login and the sessions. `Debug` redacts the client secret.
#[derive(Debug, Clone)]
pub struct BffConfig {
    /// Issuer of the login provider, exactly as in its discovery document (a trailing `/` is removed).
    pub issuer: String,
    /// Client id of this app.
    pub client_id: String,
    /// Client secret of this app.
    pub client_secret: SecretString,
    /// Redirect URI of the callback, exactly as registered.
    pub redirect_uri: String,
    /// Where the provider sends the browser after the logout (registered exactly); `None` = the
    /// provider's own page.
    pub post_logout_redirect_uri: Option<String>,
    /// `SameSite` of the session cookie.
    pub same_site: SameSite,
    /// Idle and absolute timeout of sessions.
    pub timeouts: Timeouts,
}

/// Default idle timeout (2 hours).
const IDLE_DEFAULT: u64 = 7200;
/// Default absolute timeout (10 hours, a working day).
const ABSOLUTE_DEFAULT: u64 = 36_000;

impl BffConfig {
    /// The variables of [`BffConfig::from_lookup`].
    pub const VARIABLES: &'static [ConfigVar] = &[
        ConfigVar {
            suffix: "ISSUER",
            default: None,
            description: "Issuer of the login provider (`https://…`; `http://` only for loopback hosts), \
                          exactly as in its discovery document. Required.",
        },
        ConfigVar {
            suffix: "CLIENT_ID",
            default: None,
            description: "Client id of this app at the login provider. Required.",
        },
        ConfigVar {
            suffix: "CLIENT_SECRET",
            default: None,
            description: "Client secret of this app. Required. Secret.",
        },
        ConfigVar {
            suffix: "REDIRECT_URI",
            default: None,
            description: "Callback URL of this app (`https://app.example.com/auth/callback`), exactly as \
                          registered. Required. A plain-HTTP loopback URL (local development) turns off \
                          `Secure` and the `__Host-` prefix of the cookies.",
        },
        ConfigVar {
            suffix: "POST_LOGOUT_REDIRECT_URI",
            default: None,
            description: "Where the login provider sends the browser after the logout, exactly as \
                          registered. Unset: the provider's own page.",
        },
        ConfigVar {
            suffix: "COOKIE_SAMESITE",
            default: Some("strict"),
            description: "`SameSite` of the session cookie: `strict` (links from other sites open the app \
                          without the session) or `lax`. The login cookie is always `lax`.",
        },
        ConfigVar {
            suffix: "SESSION_IDLE_SECS",
            default: Some("7200"),
            description: "A session ends after this many seconds without a request, 60–86400 (cut to \
                          the absolute timeout).",
        },
        ConfigVar {
            suffix: "SESSION_MAX_SECS",
            default: Some("36000"),
            description: "A session ends this many seconds after the login, however active, \
                          300–2592000. Bounds how long roles taken at login stay in effect without a \
                          back-channel logout.",
        },
    ];

    /// Reads the variables of [`BffConfig::VARIABLES`] with `prefix`.
    pub fn from_lookup(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let l = Lookup::new(prefix, lookup);
        let required = |suffix: &str| {
            l.get(suffix)
                .ok_or_else(|| l.invalid(suffix, "must be set"))
        };
        let url = |suffix: &str, raw: String| {
            check_url(&raw).map_err(|reason| l.error(suffix, url_kind(), reason))?;
            Ok::<_, ConfigError>(raw)
        };
        let issuer = url("ISSUER", required("ISSUER")?)?;
        let issuer = issuer.strip_suffix('/').unwrap_or(&issuer).to_owned();
        let redirect_uri = url("REDIRECT_URI", required("REDIRECT_URI")?)?;
        let post_logout_redirect_uri = l
            .get("POST_LOGOUT_REDIRECT_URI")
            .map(|raw| url("POST_LOGOUT_REDIRECT_URI", raw))
            .transpose()?;
        let same_site = match l.get("COOKIE_SAMESITE").as_deref() {
            None | Some("strict") => SameSite::Strict,
            Some("lax") => SameSite::Lax,
            Some(_) => return Err(l.invalid("COOKIE_SAMESITE", "expected `strict` or `lax`")),
        };
        let idle = l.number("SESSION_IDLE_SECS", IDLE_DEFAULT, 60..=86_400)?;
        let absolute = l.number("SESSION_MAX_SECS", ABSOLUTE_DEFAULT, 300..=2_592_000)?;
        Ok(Self {
            issuer,
            client_id: required("CLIENT_ID")?,
            client_secret: SecretString::new(required("CLIENT_SECRET")?),
            redirect_uri,
            post_logout_redirect_uri,
            same_site,
            // Both ranges fit in u32.
            timeouts: Timeouts::from_secs(idle as u32, absolute as u32),
        })
    }

    /// Whether cookies get `Secure` and the `__Host-` prefix: always, unless the redirect URI is plain
    /// HTTP (which [`BffConfig::from_lookup`] only accepts for loopback hosts).
    pub fn secure_cookies(&self) -> bool {
        Url::parse(&self.redirect_uri).map_or(true, |u| u.scheme() != "http")
    }
}

/// The error kind of a bad URL.
fn url_kind() -> z40_fundament::config::ConfigErrorKind {
    z40_fundament::config::ConfigErrorKind::InvalidUrl
}

/// `https://` URL, or `http://` to a loopback host; no user info, no fragment.
pub(crate) fn check_url(raw: &str) -> Result<(), &'static str> {
    let url = Url::parse(raw).map_err(|_| "not a URL")?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err("no user info or fragment allowed");
    }
    let loopback = match url.host() {
        Some(Host::Domain(d)) => d == "localhost",
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    match url.scheme() {
        "https" if url.host().is_some() => Ok(()),
        "http" if loopback => Ok(()),
        _ => Err("expected https:// (http:// only for loopback hosts)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use z40_fundament::config::ConfigErrorKind;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    const BASE: [(&str, &str); 4] = [
        ("AUTH_ISSUER", "https://login.example.com/"),
        ("AUTH_CLIENT_ID", "crm"),
        ("AUTH_CLIENT_SECRET", "s3cr3t"),
        ("AUTH_REDIRECT_URI", "https://crm.example.com/auth/callback"),
    ];

    fn with(extra: &[(&str, &str)]) -> Result<BffConfig, ConfigError> {
        let mut all: Vec<(&str, &str)> = BASE.to_vec();
        all.retain(|(k, _)| !extra.iter().any(|(e, _)| e == k));
        all.extend_from_slice(extra);
        BffConfig::from_lookup("AUTH", env(&all))
    }

    #[test]
    fn defaults_and_redacted_secret() {
        let c = with(&[]).unwrap();
        assert_eq!(c.issuer, "https://login.example.com");
        assert_eq!(c.client_secret.expose(), "s3cr3t");
        assert_eq!(c.same_site, SameSite::Strict);
        assert_eq!(c.timeouts, Timeouts::from_secs(7200, 36_000));
        assert_eq!(c.post_logout_redirect_uri, None);
        assert!(c.secure_cookies());
        assert!(!format!("{c:?}").contains("s3cr3t"));
    }

    #[test]
    fn optional_settings() {
        let c = with(&[
            ("AUTH_POST_LOGOUT_REDIRECT_URI", "https://crm.example.com/"),
            ("AUTH_COOKIE_SAMESITE", "lax"),
            ("AUTH_SESSION_IDLE_SECS", "900"),
            ("AUTH_SESSION_MAX_SECS", "3600"),
        ])
        .unwrap();
        assert_eq!(
            c.post_logout_redirect_uri.as_deref(),
            Some("https://crm.example.com/")
        );
        assert_eq!(c.same_site, SameSite::Lax);
        assert_eq!(c.timeouts, Timeouts::from_secs(900, 3600));
    }

    #[test]
    fn required_values_and_bad_values_are_refused() {
        for missing in [
            "AUTH_ISSUER",
            "AUTH_CLIENT_ID",
            "AUTH_CLIENT_SECRET",
            "AUTH_REDIRECT_URI",
        ] {
            let err = with(&[(missing, " ")]).unwrap_err();
            assert_eq!(err.name, missing);
        }
        let err = with(&[("AUTH_COOKIE_SAMESITE", "none")]).unwrap_err();
        assert_eq!(err.name, "AUTH_COOKIE_SAMESITE");
        let err = with(&[("AUTH_SESSION_IDLE_SECS", "5")]).unwrap_err();
        assert!(matches!(err.kind, ConfigErrorKind::OutOfRange { .. }));
        let err = with(&[("AUTH_REDIRECT_URI", "http://crm.example.com/cb?k=s3cr3t")]).unwrap_err();
        assert_eq!(err.kind, ConfigErrorKind::InvalidUrl);
        assert!(!err.to_string().contains("s3cr3t"), "value never echoed");
    }

    #[test]
    fn plain_http_only_on_loopback_and_then_without_secure_cookies() {
        let c = with(&[
            ("AUTH_ISSUER", "http://127.0.0.1:8400"),
            ("AUTH_REDIRECT_URI", "http://localhost:3000/auth/callback"),
        ])
        .unwrap();
        assert!(!c.secure_cookies());
        for ok in [
            "https://a.example",
            "http://localhost:1/x",
            "http://[::1]/",
            "http://127.0.0.2/",
        ] {
            assert_eq!(check_url(ok), Ok(()), "{ok}");
        }
        for bad in [
            "http://a.example/",
            "http://localhost.example.com/",
            "https://user:pw@a.example/",
            "https://a.example/#x",
            "ftp://localhost/",
            "javascript:alert(1)",
            "/relative",
            "https://",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }
}

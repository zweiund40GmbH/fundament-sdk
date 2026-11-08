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

/// Settings of the login and the sessions. `Debug` redacts the client secret. Built with
/// [`BffConfig::from_lookup`] or [`BffConfig::new`]; the URLs are checked again when the
/// [`super::Bff`] is built, so changing a field afterwards cannot slip past the rules.
#[derive(Debug, Clone)]
#[non_exhaustive]
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
    /// Login starts per client address and minute.
    pub login_rate_per_ip: u32,
    /// Login starts per minute, all clients together.
    pub login_rate_total: u32,
}

/// Default idle timeout (2 hours).
const IDLE_DEFAULT: u64 = 7200;
/// Default absolute timeout (10 hours, a working day).
const ABSOLUTE_DEFAULT: u64 = 36_000;
/// Default login starts per client address and minute.
const RATE_PER_IP_DEFAULT: u64 = 30;
/// Default login starts per minute in total.
const RATE_TOTAL_DEFAULT: u64 = 300;

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
            description: "Callback URL of this app (`https://app.example.com/api/auth/callback`), exactly as \
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
        ConfigVar {
            suffix: "LOGIN_RATE_PER_IP",
            default: Some("30"),
            description: "Login starts per client address and minute, 1–10000 (more get `429`). Counted \
                          per process; IPv6 by /64.",
        },
        ConfigVar {
            suffix: "LOGIN_RATE_TOTAL",
            default: Some("300"),
            description: "Login starts per minute from all clients together, 1–1000000. Bounds the rows \
                          written before anyone is authenticated. Counted per process.",
        },
    ];

    /// Settings with the defaults of [`BffConfig::VARIABLES`] for the remaining fields. The URLs follow
    /// the rules of the module docs (a trailing `/` of the issuer is removed).
    pub fn new(
        issuer: &str,
        client_id: &str,
        client_secret: SecretString,
        redirect_uri: &str,
    ) -> Result<Self, InvalidUrl> {
        check_url(issuer).map_err(|_| InvalidUrl("issuer"))?;
        check_url(redirect_uri).map_err(|_| InvalidUrl("redirect_uri"))?;
        Ok(Self {
            issuer: issuer.strip_suffix('/').unwrap_or(issuer).to_owned(),
            client_id: client_id.to_owned(),
            client_secret,
            redirect_uri: redirect_uri.to_owned(),
            post_logout_redirect_uri: None,
            same_site: SameSite::Strict,
            timeouts: Timeouts::from_secs(IDLE_DEFAULT as u32, ABSOLUTE_DEFAULT as u32),
            login_rate_per_ip: RATE_PER_IP_DEFAULT as u32,
            login_rate_total: RATE_TOTAL_DEFAULT as u32,
        })
    }

    /// Checks the URLs (issuer, redirect URI, post-logout redirect URI) against the rules of the module
    /// docs; the error names the field.
    pub fn check(&self) -> Result<(), InvalidUrl> {
        let urls = [
            ("issuer", Some(self.issuer.as_str())),
            ("redirect_uri", Some(self.redirect_uri.as_str())),
            (
                "post_logout_redirect_uri",
                self.post_logout_redirect_uri.as_deref(),
            ),
        ];
        for (field, url) in urls {
            if url.is_some_and(|u| check_url(u).is_err()) {
                return Err(InvalidUrl(field));
            }
        }
        Ok(())
    }

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
        let per_ip = l.number("LOGIN_RATE_PER_IP", RATE_PER_IP_DEFAULT, 1..=10_000)?;
        let total = l.number("LOGIN_RATE_TOTAL", RATE_TOTAL_DEFAULT, 1..=1_000_000)?;
        Ok(Self {
            issuer,
            client_id: required("CLIENT_ID")?,
            client_secret: SecretString::new(required("CLIENT_SECRET")?),
            redirect_uri,
            post_logout_redirect_uri,
            same_site,
            // All ranges fit in u32.
            timeouts: Timeouts::from_secs(idle as u32, absolute as u32),
            login_rate_per_ip: per_ip as u32,
            login_rate_total: total as u32,
        })
    }

    /// Whether cookies get `Secure` and the `__Host-` prefix: always, unless the redirect URI is plain
    /// HTTP (which [`BffConfig::from_lookup`] only accepts for loopback hosts).
    pub fn secure_cookies(&self) -> bool {
        Url::parse(&self.redirect_uri).map_or(true, |u| u.scheme() != "http")
    }
}

/// A URL of the configuration breaks the rules of the module docs; names the field, never the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid URL in {0} (https://, http:// only for loopback hosts, no user info or fragment)")]
pub struct InvalidUrl(pub &'static str);

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
#[path = "config_tests.rs"]
mod tests;

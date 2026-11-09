//! [`BffConfig`]: the client registration at the login provider and the session settings, read with
//! [`BffConfig::from_lookup`] (or [`BffConfig::from_lookup_with`] for defaults the app computes). The
//! app chooses the prefix, so existing variable names stay: with the prefix `AUTH` the variables are
//! `AUTH_ISSUER`, `AUTH_CLIENT_ID`, `AUTH_CLIENT_SECRET`, … .
//!
//! URLs must be `https://`. Plain `http://` to a loopback host (`localhost`, `127.0.0.1`, `[::1]`) is
//! for local development only and refused unless the app opts in with `allow_loopback_http`
//! ([`Defaults::allow_loopback_http`], [`BffConfig::allow_loopback_http`]), e.g. only when its
//! environment is `development`. When the redirect URI is such a plain-HTTP URL, the cookies drop
//! `Secure` and the `__Host-` prefix ([`BffConfig::secure_cookies`]) – the only relaxed setting, and only
//! where the browser would otherwise refuse the cookie.

use super::cookies::SameSite;
use super::timeouts::Timeouts;
pub use super::urls::InvalidUrl;
use super::urls::{check_url, url_kind};
use url::Url;
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
    /// Whether plain-HTTP loopback URLs are accepted (local development only; default `false`). Set
    /// with [`BffConfig::allow_loopback_http`] or [`Defaults::allow_loopback_http`].
    pub allow_loopback_http: bool,
}

/// Defaults the app computes for [`BffConfig::from_lookup_with`] (e.g. the redirect URI from its public
/// URL), used when the variable is unset or empty; and whether plain-HTTP loopback URLs are accepted.
/// `Default`: no defaults, loopback HTTP refused (what [`BffConfig::from_lookup`] uses).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Defaults {
    /// Default of `<PREFIX>_CLIENT_ID`.
    pub client_id: Option<String>,
    /// Default of `<PREFIX>_REDIRECT_URI` (checked like the variable).
    pub redirect_uri: Option<String>,
    /// Default of `<PREFIX>_POST_LOGOUT_REDIRECT_URI` (checked like the variable).
    pub post_logout_redirect_uri: Option<String>,
    /// Accept plain-HTTP loopback URLs; only for local development (e.g. `env == development`).
    pub allow_loopback_http: bool,
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
            description: "Issuer of the login provider (`https://…`; `http://` only for loopback hosts \
                          when the app allows it in development), exactly as in its discovery document. \
                          Required.",
        },
        ConfigVar {
            suffix: "CLIENT_ID",
            default: None,
            description: "Client id of this app at the login provider. Required unless the app passes \
                          a default (`from_lookup_with`).",
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
                          registered. Required unless the app passes a default (`from_lookup_with`). A \
                          plain-HTTP loopback URL (only when the app allows it in development) turns off \
                          `Secure` and the `__Host-` prefix of the cookies.",
        },
        ConfigVar {
            suffix: "POST_LOGOUT_REDIRECT_URI",
            default: None,
            description: "Where the login provider sends the browser after the logout, exactly as \
                          registered. Unset: the app's default (`from_lookup_with`), else the provider's \
                          own page.",
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
                          per process, or across instances with a shared `LoginLimitStore`; IPv6 by /64.",
        },
        ConfigVar {
            suffix: "LOGIN_RATE_TOTAL",
            default: Some("300"),
            description: "Login starts per minute from all clients together, 1–1000000. Bounds the rows \
                          written before anyone is authenticated. Counted per process, or across \
                          instances with a shared `LoginLimitStore`.",
        },
    ];

    /// Settings with the defaults of [`BffConfig::VARIABLES`] for the remaining fields. The URLs follow
    /// the rules of the module docs (a trailing `/` of the issuer is removed); a plain-HTTP loopback URL
    /// is accepted here, but [`super::BffBuilder::build`] refuses it unless
    /// [`BffConfig::allow_loopback_http`] is set.
    pub fn new(
        issuer: &str,
        client_id: &str,
        client_secret: SecretString,
        redirect_uri: &str,
    ) -> Result<Self, InvalidUrl> {
        check_url(issuer, true).map_err(|_| InvalidUrl("issuer"))?;
        check_url(redirect_uri, true).map_err(|_| InvalidUrl("redirect_uri"))?;
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
            allow_loopback_http: false,
        })
    }

    /// Accepts (`true`) or refuses (`false`, the default) plain-HTTP loopback URLs. Only for local
    /// development: pass e.g. `env == development`, never a constant `true`.
    pub fn allow_loopback_http(mut self, allow: bool) -> Self {
        self.allow_loopback_http = allow;
        self
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
            if url.is_some_and(|u| check_url(u, self.allow_loopback_http).is_err()) {
                return Err(InvalidUrl(field));
            }
        }
        Ok(())
    }

    /// Reads the variables of [`BffConfig::VARIABLES`] with `prefix`; plain-HTTP loopback URLs are
    /// refused (see [`BffConfig::from_lookup_with`]).
    pub fn from_lookup(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        Self::from_lookup_with(prefix, lookup, Defaults::default())
    }

    /// Like [`BffConfig::from_lookup`], with the app's `defaults` for unset variables and its choice
    /// on plain-HTTP loopback URLs.
    ///
    /// ```
    /// use z40_fundament_http::bff::{BffConfig, Defaults};
    ///
    /// # fn main() -> Result<(), z40_fundament::config::ConfigError> {
    /// let public_url = "http://localhost:3000";
    /// let development = true;
    /// let env = |k: &str| match k {
    ///     "AUTH_ISSUER" => Some("http://localhost:8400".to_owned()),
    ///     "AUTH_CLIENT_SECRET" => Some("s3cr3t".to_owned()),
    ///     _ => None,
    /// };
    /// let defaults = Defaults {
    ///     client_id: Some("crm".into()),
    ///     redirect_uri: Some(format!("{public_url}/api/auth/callback")),
    ///     post_logout_redirect_uri: Some(format!("{public_url}/")),
    ///     allow_loopback_http: development,
    /// };
    /// let config = BffConfig::from_lookup_with("AUTH", env, defaults)?;
    /// assert_eq!(config.client_id, "crm");
    /// # Ok(())
    /// # }
    /// ```
    pub fn from_lookup_with(
        prefix: &str,
        lookup: impl Fn(&str) -> Option<String>,
        defaults: Defaults,
    ) -> Result<Self, ConfigError> {
        let l = Lookup::new(prefix, lookup);
        let loopback = defaults.allow_loopback_http;
        let required = |suffix: &str, default: Option<String>| {
            l.get(suffix)
                .or(default)
                .ok_or_else(|| l.invalid(suffix, "must be set"))
        };
        let url = |suffix: &str, raw: String| {
            check_url(&raw, loopback).map_err(|reason| l.error(suffix, url_kind(), reason))?;
            Ok::<_, ConfigError>(raw)
        };
        let issuer = url("ISSUER", required("ISSUER", None)?)?;
        let issuer = issuer.strip_suffix('/').unwrap_or(&issuer).to_owned();
        let redirect_uri = url(
            "REDIRECT_URI",
            required("REDIRECT_URI", defaults.redirect_uri)?,
        )?;
        let post_logout_redirect_uri = l
            .get("POST_LOGOUT_REDIRECT_URI")
            .or(defaults.post_logout_redirect_uri)
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
            client_id: required("CLIENT_ID", defaults.client_id)?,
            client_secret: SecretString::new(required("CLIENT_SECRET", None)?),
            redirect_uri,
            post_logout_redirect_uri,
            same_site,
            // All ranges fit in u32.
            timeouts: Timeouts::from_secs(idle as u32, absolute as u32),
            login_rate_per_ip: per_ip as u32,
            login_rate_total: total as u32,
            allow_loopback_http: loopback,
        })
    }

    /// Whether cookies get `Secure` and the `__Host-` prefix: always, unless the redirect URI is plain
    /// HTTP (only accepted for loopback hosts with `allow_loopback_http`).
    pub fn secure_cookies(&self) -> bool {
        Url::parse(&self.redirect_uri).map_or(true, |u| u.scheme() != "http")
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

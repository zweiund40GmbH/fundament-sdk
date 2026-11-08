//! The two cookies of the login: the **session cookie** `__Host-<name>_sid` and the short-lived
//! **login cookie** `__Host-<name>_login`, which ties a started login to the browser that started it.
//!
//! Both are `HttpOnly` (no script can read them), `Secure` and `Path=/` without `Domain` – the
//! `__Host-` prefix makes browsers enforce exactly that, so neither a sibling subdomain nor a plain-HTTP
//! page can set or overwrite them. The session cookie is `SameSite=Strict` by default (configurable to
//! `Lax`); the login cookie is always `SameSite=Lax`, because it must come back with the top-level
//! redirect from the login provider. Only when the app itself runs on plain HTTP on a loopback host (local
//! development, see [`super::BffConfig`]) are `Secure` and the prefix left out, since browsers would
//! refuse them there.

use axum::http::{HeaderMap, HeaderValue, header};

/// `SameSite` of the session cookie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SameSite {
    /// Sent only with requests started by the app's own site. A link from another site (mail, chat)
    /// opens the app without the session; the app's own `fetch` calls carry it.
    #[default]
    Strict,
    /// Also sent with top-level navigations from other sites (`GET` only).
    Lax,
}

impl SameSite {
    /// The attribute value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "Strict",
            Self::Lax => "Lax",
        }
    }
}

/// The base name is not 1–32 characters of `a-z 0-9 _ -`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid cookie name (1-32 characters of a-z, 0-9, _, -)")]
pub struct InvalidCookieName;

/// Names and attributes of the two cookies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookies {
    session: String,
    login: String,
    same_site: SameSite,
    secure: bool,
}

impl Cookies {
    /// The cookies `__Host-<base>_sid` and `__Host-<base>_login` (without the prefix and `Secure` when
    /// `secure` is `false`).
    pub fn new(base: &str, same_site: SameSite, secure: bool) -> Result<Self, InvalidCookieName> {
        let ok = (1..=32).contains(&base.len())
            && base
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !ok {
            return Err(InvalidCookieName);
        }
        let prefix = if secure { "__Host-" } else { "" };
        Ok(Self {
            session: format!("{prefix}{base}_sid"),
            login: format!("{prefix}{base}_login"),
            same_site,
            secure,
        })
    }

    /// Name of the session cookie.
    pub fn session_name(&self) -> &str {
        &self.session
    }

    /// Name of the login cookie.
    pub fn login_name(&self) -> &str {
        &self.login
    }

    /// `Set-Cookie` value of the session cookie with `value`, valid for `max_age_secs` (0 deletes it).
    pub fn session_cookie(&self, value: &str, max_age_secs: i64) -> String {
        self.build(&self.session, value, max_age_secs, self.same_site)
    }

    /// `Set-Cookie` value of the login cookie (always `SameSite=Lax`).
    pub fn login_cookie(&self, value: &str, max_age_secs: i64) -> String {
        self.build(&self.login, value, max_age_secs, SameSite::Lax)
    }

    /// The value of the session cookie in `headers`.
    pub fn session_value(&self, headers: &HeaderMap) -> Option<String> {
        cookie_value(headers, &self.session)
    }

    /// The value of the login cookie in `headers`.
    pub fn login_value(&self, headers: &HeaderMap) -> Option<String> {
        cookie_value(headers, &self.login)
    }

    /// One `Set-Cookie` value. Values are cookie tokens of this module (base64url) or empty.
    fn build(&self, name: &str, value: &str, max_age: i64, same_site: SameSite) -> String {
        let mut s = format!(
            "{name}={value}; Path=/; Max-Age={}; SameSite={}; HttpOnly",
            max_age.max(0),
            same_site.as_str()
        );
        if self.secure {
            s.push_str("; Secure");
        }
        s
    }
}

/// The first non-empty value of cookie `name` in the `Cookie` headers.
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Appends `cookie` as a `Set-Cookie` header (values built here are always valid header values).
pub(crate) fn append(headers: &mut HeaderMap, cookie: &str) {
    if let Ok(v) = HeaderValue::from_str(cookie) {
        headers.append(header::SET_COOKIE, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_cookies_use_the_host_prefix() {
        let c = Cookies::new("crm", SameSite::Strict, true).unwrap();
        assert_eq!(c.session_name(), "__Host-crm_sid");
        assert_eq!(c.login_name(), "__Host-crm_login");
        assert_eq!(
            c.session_cookie("v", 600),
            "__Host-crm_sid=v; Path=/; Max-Age=600; SameSite=Strict; HttpOnly; Secure"
        );
        assert_eq!(
            c.login_cookie("v", 600),
            "__Host-crm_login=v; Path=/; Max-Age=600; SameSite=Lax; HttpOnly; Secure"
        );
        let lax = Cookies::new("crm", SameSite::Lax, true).unwrap();
        assert!(
            lax.session_cookie("", 0)
                .contains("Max-Age=0; SameSite=Lax")
        );
        assert!(!c.session_cookie("v", 1).contains("Domain"));
    }

    #[test]
    fn plain_http_development_drops_prefix_and_secure() {
        let c = Cookies::new("crm", SameSite::Strict, false).unwrap();
        assert_eq!(
            c.session_cookie("v", -5),
            "crm_sid=v; Path=/; Max-Age=0; SameSite=Strict; HttpOnly"
        );
    }

    #[test]
    fn base_names_are_checked() {
        for bad in ["", "Crm", "a b", "a;b", "a=b", &"a".repeat(33), "ä"] {
            assert_eq!(
                Cookies::new(bad, SameSite::Lax, true),
                Err(InvalidCookieName),
                "{bad:?}"
            );
        }
        assert!(Cookies::new("my-app_2", SameSite::Lax, true).is_ok());
    }

    #[test]
    fn values_are_read_from_all_cookie_headers() {
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, HeaderValue::from_static("a=1; crm_sid= "));
        h.append(
            header::COOKIE,
            HeaderValue::from_static("__Host-crm_sid=abc ; x=2"),
        );
        let c = Cookies::new("crm", SameSite::Strict, true).unwrap();
        assert_eq!(c.session_value(&h).as_deref(), Some("abc"));
        assert_eq!(c.login_value(&h), None);
        assert_eq!(cookie_value(&h, "crm_sid"), None, "empty counts as absent");
        assert_eq!(cookie_value(&h, "a").as_deref(), Some("1"));
    }
}

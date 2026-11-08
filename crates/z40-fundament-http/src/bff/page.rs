//! Failed logins: [`LoginFailure`] (what went wrong, generic) and the built-in page. Browser
//! navigations (login, callback) get an HTML page, not JSON. The texts are German fixed texts like the
//! other wire texts of this crate, never input and never details of the check – a rejected login says
//! only that it was rejected.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};

/// Text of an expired, reused or foreign login.
pub const MSG_EXPIRED: &str =
    "Die Anmeldung ist abgelaufen oder wurde schon verwendet. Bitte erneut anmelden.";
/// Text of a rejected login.
pub const MSG_REJECTED: &str = "Die Anmeldung wurde abgelehnt. Bitte erneut anmelden.";
/// Text when the login provider cannot be reached or its answer not be checked.
pub const MSG_UNAVAILABLE: &str =
    "Die Anmeldung ist gerade nicht möglich. Bitte versuchen Sie es später erneut.";
/// Text of an internal failure.
pub const MSG_INTERNAL: &str = "Interner Fehler. Bitte versuchen Sie es später erneut.";

/// The app's login hook refuses the login (e.g. no role for this app). The message is a fixed text of
/// the app and is shown to the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginDenied {
    status: StatusCode,
    message: &'static str,
}

impl LoginDenied {
    /// A refusal with `status` and the fixed text `message`.
    pub fn new(status: StatusCode, message: &'static str) -> Self {
        Self { status, message }
    }

    /// `403` with `message` (e.g. "no access to this app").
    pub fn forbidden(message: &'static str) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }

    /// `400` with the generic rejection text.
    pub fn rejected() -> Self {
        Self::new(StatusCode::BAD_REQUEST, MSG_REJECTED)
    }

    /// `502` when the hook could not check the login (e.g. the access token could not be verified).
    pub fn unavailable() -> Self {
        Self::new(StatusCode::BAD_GATEWAY, MSG_UNAVAILABLE)
    }

    /// The status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The text.
    pub fn message(&self) -> &'static str {
        self.message
    }
}

/// Why a login did not end in a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoginFailure {
    /// No, an expired or an already used login state (also a callback in another browser).
    Expired,
    /// The provider's answer was rejected (`state`, `iss`, `nonce`, signature, error response …).
    Rejected,
    /// The login provider is unreachable.
    Unavailable,
    /// Storage or randomness failed.
    Internal,
    /// The app's login hook refused.
    Denied(LoginDenied),
}

impl LoginFailure {
    /// The HTTP status.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Expired | Self::Rejected => StatusCode::BAD_REQUEST,
            Self::Unavailable => StatusCode::BAD_GATEWAY,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Denied(d) => d.status,
        }
    }

    /// The fixed text.
    pub fn message(&self) -> &'static str {
        match self {
            Self::Expired => MSG_EXPIRED,
            Self::Rejected => MSG_REJECTED,
            Self::Unavailable => MSG_UNAVAILABLE,
            Self::Internal => MSG_INTERNAL,
            Self::Denied(d) => d.message,
        }
    }
}

/// The built-in page: status, a short German HTML document with the text and no link (the app's start
/// page offers the login again).
pub(crate) fn default_page(failure: &LoginFailure) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"de\"><head><meta charset=\"utf-8\"><title>Anmeldung</title></head>\
         <body><h1>Anmeldung nicht möglich</h1><p>{}</p><p><a href=\"/\">Zur Startseite</a></p></body></html>",
        escape_html(failure.message())
    );
    (failure.status(), Html(body)).into_response()
}

/// Sets `Cache-Control: no-store` (every answer of the login routes).
pub(crate) fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// Escapes `&`, `<`, `>`, `"`, `'` (the texts are fixed, but an app's own text is not checked).
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_and_texts() {
        assert_eq!(LoginFailure::Expired.status(), StatusCode::BAD_REQUEST);
        assert_eq!(LoginFailure::Unavailable.status(), StatusCode::BAD_GATEWAY);
        let denied = LoginFailure::Denied(LoginDenied::forbidden("Kein Zugang."));
        assert_eq!(
            (denied.status(), denied.message()),
            (StatusCode::FORBIDDEN, "Kein Zugang.")
        );
        assert_eq!(LoginDenied::rejected().message(), MSG_REJECTED);
    }

    #[test]
    fn page_escapes_the_text() {
        assert_eq!(
            escape_html("<a href='x'>&\""),
            "&lt;a href=&#39;x&#39;&gt;&amp;&quot;"
        );
        let r = default_page(&LoginFailure::Denied(LoginDenied::forbidden("<b>")));
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
    }
}

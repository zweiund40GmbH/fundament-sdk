//! `POST <base>/logout` (the session ends here; the browser is sent to the provider's logout) and
//! `POST <base>/backchannel-logout` (the provider ends the sessions of a `sid` or `sub`).

use super::page::no_store;
use super::token::{self, TokenHash};
use super::{Bff, SessionData, cookies};
use crate::csrf::default_rejection;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use z40_auth::LogoutEvent;
use z40_auth::axum::{LogoutToken, logout_response};
use z40_fundament::StoreError;

/// Name of the fetch metadata header that tells where a request came from.
const SEC_FETCH_SITE: &str = "sec-fetch-site";

/// Whether a request may log out as far as fetch metadata tells: without `Sec-Fetch-Site` (older
/// browsers, other clients) yes, with it only from the app's own origin. Defence in depth; the app's
/// `Csrf` layer must cover the route as well.
pub(crate) fn same_origin_or_unknown(headers: &HeaderMap) -> bool {
    headers
        .get(SEC_FETCH_SITE)
        .is_none_or(|v| v.as_bytes() == b"same-origin")
}

/// Ends the session of the request (if any) and answers `200 {"logoutUrl": "…"}`: the provider's
/// logout URL (with the post-logout redirect URI, without `id_token_hint`, so the provider may ask to
/// confirm), or `/` if the provider is unreachable. The browser navigates there. The answer is JSON
/// because the call comes from the app's script with the CSRF header. A request another site started
/// (`Sec-Fetch-Site` other than `same-origin`) gets the `403` of [`crate::csrf`].
pub(crate) async fn logout<T: SessionData>(
    State(bff): State<Bff<T>>,
    headers: HeaderMap,
) -> Response {
    if !same_origin_or_unknown(&headers) {
        return default_rejection();
    }
    let i = &bff.inner;
    if let Some(value) = i
        .cookies
        .session_value(&headers)
        .filter(|v| token::is_well_formed(v))
        && let Err(e) = i.store.delete(&TokenHash::of(&value)).await
    {
        tracing::error!(error = %e, "ending the session failed");
        return super::SessionRejection::Internal.into_response();
    }
    let post_logout = i.config.post_logout_redirect_uri.as_deref();
    let url = match bff.client().await {
        Ok(client) => client
            .logout_url(None, post_logout, None)
            .map_or_else(|_| "/".to_owned(), |u| u.to_string()),
        Err(_) => "/".to_owned(),
    };
    let mut out = HeaderMap::new();
    cookies::append(&mut out, &i.cookies.session_cookie("", 0));
    no_store((out, Json(json!({ "logoutUrl": url }))).into_response())
}

/// Verifies the logout token and ends the sessions of its `sid` (or, without `sid`, of its `sub`).
/// `200` once they are ended, `400` for any failure (OIDC Back-Channel Logout §2.8); no details.
pub(crate) async fn backchannel<T: SessionData>(
    State(bff): State<Bff<T>>,
    LogoutToken(token): LogoutToken,
) -> Response {
    let Ok(client) = bff.client().await else {
        return logout_response(false);
    };
    let event = match client.verify_backchannel_logout(&token).await {
        Ok(event) => event,
        Err(e) => {
            tracing::warn!(error = %e, "back-channel logout rejected");
            return logout_response(false);
        }
    };
    match end_sessions(&bff, &event).await {
        Ok(count) => {
            tracing::info!(count, "back-channel logout");
            logout_response(true)
        }
        Err(e) => {
            tracing::error!(error = %e, "back-channel logout failed");
            logout_response(false)
        }
    }
}

/// The sessions a logout event ends: those of its `sid`; if none has that `sid` and the token names a
/// `sub`, the sessions of that `sub` stored without `sid` (the ID token at login carried none). Without
/// `sid`, all sessions of `sub`.
async fn end_sessions<T: SessionData>(bff: &Bff<T>, e: &LogoutEvent) -> Result<u64, StoreError> {
    let store = &bff.inner.store;
    match (&e.sid, &e.sub) {
        (Some(sid), sub) => {
            let n = store.delete_by_sid(sid).await?;
            match sub {
                Some(sub) if n == 0 => store.delete_by_sub_without_sid(sub).await,
                _ => Ok(n),
            }
        }
        (None, Some(sub)) => store.delete_by_sub(sub).await,
        (None, None) => Ok(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn only_same_origin_fetch_metadata_passes() {
        let mut h = HeaderMap::new();
        assert!(same_origin_or_unknown(&h), "header absent");
        for (value, ok) in [
            ("same-origin", true),
            ("same-site", false),
            ("cross-site", false),
            ("none", false),
            ("Same-Origin", false),
        ] {
            h.insert(SEC_FETCH_SITE, HeaderValue::from_static(value));
            assert_eq!(same_origin_or_unknown(&h), ok, "{value}");
        }
    }
}

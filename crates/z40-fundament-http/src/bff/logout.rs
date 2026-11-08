//! `POST <base>/logout` (the session ends here; the browser is sent to the provider's logout) and
//! `POST <base>/backchannel-logout` (the provider ends the sessions of a `sid` or `sub`).

use super::page::no_store;
use super::token::{self, Purpose, TokenHash};
use super::{Bff, BffError, SessionData, cookies};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use z40_auth::axum::{LogoutToken, logout_response};

/// Ends the session of the request (if any) and answers `200 {"logoutUrl": "…"}`: the provider's
/// logout URL (with `id_token_hint` if the ID token was kept, and the post-logout redirect URI), or `/`
/// if the provider is unreachable. The browser navigates there. The answer is JSON because the call
/// comes from the app's script with the CSRF header.
pub(crate) async fn logout<T: SessionData>(
    State(bff): State<Bff<T>>,
    headers: HeaderMap,
) -> Response {
    let i = &bff.inner;
    let mut hint = None;
    if let Some(value) = i
        .cookies
        .session_value(&headers)
        .filter(|v| token::is_well_formed(v))
    {
        let id = TokenHash::of(&value);
        match end(&bff, &id).await {
            Ok(sealed) => {
                hint = sealed
                    .and_then(|s| token::open(&value, Purpose::IdToken, &s))
                    .and_then(|t| String::from_utf8(t).ok());
            }
            Err(e) => {
                tracing::error!(error = %e, "ending the session failed");
                return super::SessionRejection::Internal.into_response();
            }
        }
    }
    let post_logout = i.config.post_logout_redirect_uri.as_deref();
    let url = match bff.client().await {
        Ok(client) => client
            .logout_url(hint.as_deref(), post_logout, None)
            .map_or_else(|_| "/".to_owned(), |u| u.to_string()),
        Err(_) => "/".to_owned(),
    };
    let mut out = HeaderMap::new();
    cookies::append(&mut out, &i.cookies.session_cookie("", 0));
    no_store((out, Json(json!({ "logoutUrl": url }))).into_response())
}

/// Deletes session `id`; returns its sealed ID token, if it had one.
async fn end<T: SessionData>(bff: &Bff<T>, id: &TokenHash) -> Result<Option<Vec<u8>>, BffError> {
    let store = &bff.inner.store;
    let sealed = store.load(id).await?.and_then(|r| r.id_token);
    store.delete(id).await?;
    Ok(sealed)
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
    let store = &bff.inner.store;
    let ended = match (&event.sid, &event.sub) {
        (Some(sid), _) => store.delete_by_sid(sid).await,
        (None, Some(sub)) => store.delete_by_sub(sub).await,
        (None, None) => Ok(0),
    };
    match ended {
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

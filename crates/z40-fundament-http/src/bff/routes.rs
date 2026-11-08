//! `GET <base>/login` and `GET <base>/callback` (Authorization Code + PKCE with `z40-auth`), and the
//! router of all four routes.
//!
//! The login state is kept in the store under the hash of a random login cookie, sealed with a key
//! derived from that cookie, valid for [`LOGIN_TTL_SECS`] and taken (deleted) at the first callback
//! whatever its outcome. The callback therefore only completes in the browser that started the login
//! (no login CSRF), at most once, and `state` and `nonce` are compared by `z40-auth` against values the
//! attacker never saw.

use super::page::{LoginFailure, no_store};
use super::return_to;
use super::store::{PendingLogin, SessionRecord};
use super::token::{self, TokenHash};
use super::{Bff, BffError, DATA_MAX, SID_MAX, SUB_MAX, SessionData, cookies};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use chrono::{TimeDelta, Utc};
use z40_auth::{CallbackParams, Error as AuthError, LoginState};

/// Lifetime of a started login.
pub const LOGIN_TTL_SECS: i64 = 600;

impl<T: SessionData> Bff<T> {
    /// The routes `GET <base>/login`, `GET <base>/callback`, `POST <base>/logout` and
    /// `POST <base>/backchannel-logout`, ready to `merge` into the app's router (any state type).
    pub fn router<S: Clone + Send + Sync + 'static>(&self) -> Router<S> {
        let base = &self.inner.base;
        Router::new()
            .route(&format!("{base}/login"), get(login::<T>))
            .route(&format!("{base}/callback"), get(callback::<T>))
            .route(&format!("{base}/logout"), post(super::logout::logout::<T>))
            .route(
                &format!("{base}/backchannel-logout"),
                post(super::logout::backchannel::<T>),
            )
            .with_state(self.clone())
    }

    /// The page of a failed login, with the login cookie cleared.
    fn failure(&self, headers: HeaderMap, failure: LoginFailure) -> Response {
        let page = (self.inner.error_page)(&failure);
        let (mut parts, body) = page.into_parts();
        append_all(&mut parts.headers, headers);
        if failure == LoginFailure::RateLimited {
            let wait = HeaderValue::from_static("60");
            parts.headers.insert(header::RETRY_AFTER, wait);
        }
        no_store(Response::from_parts(parts, body))
    }
}

/// Appends every value of `from` to `to`, keeping values `to` already has (e.g. two `Set-Cookie`).
fn append_all(to: &mut HeaderMap, from: HeaderMap) {
    for (name, value) in &from {
        to.append(name.clone(), value.clone());
    }
}

/// The single `returnTo` of the query (`None` if absent or repeated).
fn return_to_param(uri: &Uri) -> Option<String> {
    let query = uri.query().unwrap_or("");
    let mut values = url::form_urlencoded::parse(query.as_bytes())
        .filter(|(k, _)| k == "returnTo")
        .map(|(_, v)| v.into_owned());
    let first = values.next()?;
    values.next().is_none().then_some(first)
}

/// `GET <base>/login?returnTo=`: checks the login limit (`429` without writing anything), stores the
/// login state and redirects to the provider.
async fn login<T: SessionData>(State(bff): State<Bff<T>>, request: Request) -> Response {
    let (parts, _) = request.into_parts();
    if bff.inner.limit.as_ref().is_some_and(|l| !l.allow(&parts)) {
        return bff.failure(HeaderMap::new(), LoginFailure::RateLimited);
    }
    match start(&bff, &parts.uri).await {
        Ok(res) => no_store(res),
        Err(failure) => bff.failure(HeaderMap::new(), failure),
    }
}

/// The steps of the login start.
async fn start<T: SessionData>(bff: &Bff<T>, uri: &Uri) -> Result<Response, LoginFailure> {
    let i = &bff.inner;
    let client = bff.client().await.map_err(|_| LoginFailure::Unavailable)?;
    let scopes: Vec<&str> = i.scopes.iter().map(String::as_str).collect();
    let request = client
        .begin_login(&i.config.redirect_uri, &scopes, &[])
        .map_err(|e| {
            tracing::error!(error = %e, "login could not be started");
            LoginFailure::Internal
        })?;
    let value = token::generate().map_err(|_| LoginFailure::Internal)?;
    let state = serde_json::to_vec(&request.state).map_err(|_| LoginFailure::Internal)?;
    let pending = PendingLogin {
        state: token::seal(&value, &state).map_err(|_| LoginFailure::Internal)?,
        return_to: return_to::sanitize(return_to_param(uri).as_deref()),
        expires_at: Utc::now() + TimeDelta::seconds(LOGIN_TTL_SECS),
    };
    i.store
        .put_login(&TokenHash::of(&value), &pending)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "storing the login state failed");
            LoginFailure::Internal
        })?;
    let mut headers = HeaderMap::new();
    cookies::append(
        &mut headers,
        &i.cookies.login_cookie(&value, LOGIN_TTL_SECS),
    );
    Ok((headers, Redirect::to(request.url.as_str())).into_response())
}

/// `GET <base>/callback`: verifies the answer, creates the session, redirects to the return path.
async fn callback<T: SessionData>(
    State(bff): State<Bff<T>>,
    request_headers: HeaderMap,
    uri: Uri,
) -> Response {
    let mut headers = HeaderMap::new();
    cookies::append(&mut headers, &bff.inner.cookies.login_cookie("", 0));
    match finish(&bff, &request_headers, &uri).await {
        Ok((session_cookie, back)) => {
            cookies::append(&mut headers, &session_cookie);
            no_store((headers, Redirect::to(&back)).into_response())
        }
        Err(failure) => bff.failure(headers, failure),
    }
}

/// Logs a store failure and maps it to [`LoginFailure::Internal`].
fn internal(e: impl std::fmt::Display) -> LoginFailure {
    tracing::error!(error = %e, "login failed");
    LoginFailure::Internal
}

/// The provider's or the network's fault (try later) versus a rejected answer.
fn auth_failure(e: &AuthError) -> LoginFailure {
    match e {
        AuthError::Transport(_)
        | AuthError::KeysUnavailable
        | AuthError::InvalidResponse
        | AuthError::RateLimited { .. } => LoginFailure::Unavailable,
        AuthError::Status(s) if *s >= 500 => LoginFailure::Unavailable,
        _ => LoginFailure::Rejected,
    }
}

/// The steps of the callback; returns the session cookie and the return path.
async fn finish<T: SessionData>(
    bff: &Bff<T>,
    request_headers: &HeaderMap,
    uri: &Uri,
) -> Result<(String, String), LoginFailure> {
    let i = &bff.inner;
    let value = i
        .cookies
        .login_value(request_headers)
        .filter(|v| token::is_well_formed(v))
        .ok_or(LoginFailure::Expired)?;
    let pending = i
        .store
        .take_login(&TokenHash::of(&value))
        .await
        .map_err(internal)?
        .filter(|p| Utc::now() < p.expires_at)
        .ok_or(LoginFailure::Expired)?;
    let state: LoginState = token::open(&value, &pending.state)
        .and_then(|plain| serde_json::from_slice(&plain).ok())
        .ok_or(LoginFailure::Expired)?;
    let params = CallbackParams::from_query(uri.query().unwrap_or(""))
        .map_err(|_| LoginFailure::Rejected)?;
    let client = bff.client().await.map_err(|_| LoginFailure::Unavailable)?;
    let login = client.complete_login(&state, &params).await.map_err(|e| {
        tracing::warn!(error = %e, "login rejected");
        auth_failure(&e)
    })?;
    let (sub, sid) = (login.claims.sub.clone(), login.claims.sid.clone());
    if sub.len() > SUB_MAX || sid.as_ref().is_some_and(|s| s.len() > SID_MAX) {
        tracing::warn!("login rejected: sub or sid too long");
        return Err(LoginFailure::Rejected);
    }
    let data = (i.on_login)(client, login)
        .await
        .map_err(LoginFailure::Denied)?;
    let data = serde_json::to_string(&data)
        .ok()
        .filter(|d| d.len() <= DATA_MAX)
        .ok_or_else(|| internal(BffError::Data))?;
    // Rotation: the session the browser brought along ends; the new one gets a fresh id.
    if let Some(old) = i.cookies.session_value(request_headers) {
        i.store
            .delete(&TokenHash::of(&old))
            .await
            .map_err(internal)?;
    }
    let new = token::generate().map_err(internal)?;
    let now = Utc::now();
    let record = SessionRecord {
        sub,
        sid,
        data,
        created_at: now,
        last_seen_at: now,
        expires_at: i.config.timeouts.expires_at(now, now),
    };
    i.store
        .create(&TokenHash::of(&new), &record)
        .await
        .map_err(internal)?;
    tracing::info!("login");
    let max_age = i.config.timeouts.absolute().num_seconds();
    Ok((
        i.cookies.session_cookie(&new, max_age),
        return_to::sanitize(Some(&pending.return_to)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_to_must_be_given_once() {
        let uri = |s: &str| s.parse::<Uri>().unwrap();
        assert_eq!(
            return_to_param(&uri("/auth/login?returnTo=%2Forders%3Fa%3D1")).as_deref(),
            Some("/orders?a=1")
        );
        assert_eq!(return_to_param(&uri("/auth/login")), None);
        assert_eq!(
            return_to_param(&uri("/auth/login?returnTo=/a&returnTo=//evil")),
            None
        );
        assert_eq!(
            return_to_param(&uri("/auth/login?x=1&returnTo=/a")).as_deref(),
            Some("/a")
        );
    }

    #[test]
    fn all_header_values_are_kept() {
        use axum::http::{HeaderValue, header::SET_COOKIE};
        let mut to = HeaderMap::new();
        to.append(SET_COOKIE, HeaderValue::from_static("page=1"));
        let mut from = HeaderMap::new();
        from.append(SET_COOKIE, HeaderValue::from_static("a=1"));
        from.append(SET_COOKIE, HeaderValue::from_static("b=2"));
        append_all(&mut to, from);
        let all: Vec<_> = to.get_all(SET_COOKIE).iter().collect();
        assert_eq!(all, ["page=1", "a=1", "b=2"]);
    }

    #[test]
    fn provider_trouble_is_unavailable_and_everything_else_rejected() {
        assert_eq!(
            auth_failure(&AuthError::KeysUnavailable),
            LoginFailure::Unavailable
        );
        assert_eq!(
            auth_failure(&AuthError::Status(503)),
            LoginFailure::Unavailable
        );
        assert_eq!(
            auth_failure(&AuthError::Status(400)),
            LoginFailure::Rejected
        );
        assert_eq!(
            auth_failure(&AuthError::StateMismatch),
            LoginFailure::Rejected
        );
        assert_eq!(
            auth_failure(&AuthError::IssuerMismatch),
            LoginFailure::Rejected
        );
    }
}

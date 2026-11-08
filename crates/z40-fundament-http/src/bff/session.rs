//! The signed-in session of a request: [`Bff::session`] and the extractor [`Session`]. The cookie value
//! is checked for form, looked up by its hash, judged against the timeouts (an expired session is
//! deleted) and its last use recorded when due.

use super::page::no_store;
use super::token::{self, TokenHash};
use super::{Bff, BffError, SessionData};
use axum::Json;
use axum::extract::{FromRef, FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde_json::json;

/// A valid session. `Debug` shows only the times (`sub` and `data` are personal data).
pub struct Session<T> {
    /// Subject at the login provider.
    pub sub: String,
    /// Session id of the login provider.
    pub sid: Option<String>,
    /// The app's session data.
    pub data: T,
    /// Login time.
    pub created_at: DateTime<Utc>,
    /// End of the session if no further request comes (idle timeout, at most the absolute one).
    pub expires_at: DateTime<Utc>,
    id: TokenHash,
}

impl<T> Session<T> {
    /// The hash of the session cookie (the key in the store).
    pub fn id(&self) -> &TokenHash {
        &self.id
    }
}

impl<T> std::fmt::Debug for Session<T> {
    /// Times only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl<T: SessionData> Bff<T> {
    /// The session of a request with `headers`; `None` without a valid session cookie.
    pub async fn session(&self, headers: &HeaderMap) -> Result<Option<Session<T>>, BffError> {
        let Some(value) = self
            .inner
            .cookies
            .session_value(headers)
            .filter(|v| token::is_well_formed(v))
        else {
            return Ok(None);
        };
        self.session_by_id(&TokenHash::of(&value), Utc::now()).await
    }

    /// The session `id` as of `now`; deletes it when expired or unreadable.
    pub(crate) async fn session_by_id(
        &self,
        id: &TokenHash,
        now: DateTime<Utc>,
    ) -> Result<Option<Session<T>>, BffError> {
        let store = &self.inner.store;
        let Some(rec) = store.load(id).await? else {
            return Ok(None);
        };
        let t = self.inner.config.timeouts;
        if now >= rec.expires_at || !t.is_valid(rec.created_at, rec.last_seen_at, now) {
            store.delete(id).await?;
            return Ok(None);
        }
        let Ok(data) = serde_json::from_str::<T>(&rec.data) else {
            tracing::warn!("session data no longer readable; session ended");
            store.delete(id).await?;
            return Ok(None);
        };
        let mut expires_at = rec.expires_at;
        if t.touch_due(rec.last_seen_at, now) {
            expires_at = t.expires_at(rec.created_at, now);
            store.touch(id, now, expires_at).await?;
        }
        Ok(Some(Session {
            sub: rec.sub,
            sid: rec.sid,
            data,
            created_at: rec.created_at,
            expires_at,
            id: *id,
        }))
    }
}

/// Rejection of the [`Session`] extractor, in the error form of the endpoints of `z40-fundament`
/// (`{"code","message"}`, German fixed text, `Cache-Control: no-store`).
#[derive(Debug)]
#[non_exhaustive]
pub enum SessionRejection {
    /// `401 UNAUTHENTICATED` – no valid session.
    Unauthenticated,
    /// `500 INTERNAL` – the store failed (logged, details never sent).
    Internal,
}

impl IntoResponse for SessionRejection {
    /// `401` or `500` with `{"code","message"}`.
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                "UNAUTHENTICATED",
                "Anmeldung erforderlich.",
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL",
                super::page::MSG_INTERNAL,
            ),
        };
        no_store((status, Json(json!({ "code": code, "message": message }))).into_response())
    }
}

/// Loads the session for the extractors; store failures are logged and become `Internal`.
async fn extract<T: SessionData>(
    bff: &Bff<T>,
    parts: &Parts,
) -> Result<Option<Session<T>>, SessionRejection> {
    bff.session(&parts.headers).await.map_err(|e| {
        tracing::error!(error = %e, "loading the session failed");
        SessionRejection::Internal
    })
}

impl<S, T> FromRequestParts<S> for Session<T>
where
    S: Send + Sync,
    T: SessionData,
    Bff<T>: FromRef<S>,
{
    type Rejection = SessionRejection;

    /// The session, or `401`.
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        extract(&Bff::<T>::from_ref(state), parts)
            .await?
            .ok_or(SessionRejection::Unauthenticated)
    }
}

impl<S, T> OptionalFromRequestParts<S> for Session<T>
where
    S: Send + Sync,
    T: SessionData,
    Bff<T>: FromRef<S>,
{
    type Rejection = SessionRejection;

    /// The session if there is one; only a store failure rejects.
    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        extract(&Bff::<T>::from_ref(state), parts).await
    }
}

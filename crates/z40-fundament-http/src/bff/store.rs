//! [`SessionStore`]: where sessions and started logins live, implemented by the app (or by
//! `bff-mysql`). Keys are [`TokenHash`]es, never cookie values. The store keeps no rules: expiry, single
//! use of a login and the timeouts are decided by the caller in this crate; the store only has to
//! persist what it gets and delete what it is told to.
//!
//! Every instance of the app must see the same store (sessions are not kept in the process).

use super::token::TokenHash;
use chrono::{DateTime, Utc};
use std::future::Future;
use std::pin::Pin;
use z40_fundament::StoreError;

/// A session as stored. `Debug` shows only the times (`sub` and `data` are personal data).
#[derive(Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// Subject at the login provider.
    pub sub: String,
    /// Session id of the login provider (`sid` of the ID token), for back-channel logout.
    pub sid: Option<String>,
    /// The app's session data as JSON (what its login hook returned).
    pub data: String,
    /// Login time.
    pub created_at: DateTime<Utc>,
    /// Last recorded use.
    pub last_seen_at: DateTime<Utc>,
    /// End of the session under the timeouts at the last write; for the store's cleanup.
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for SessionRecord {
    /// Times only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRecord")
            .field("created_at", &self.created_at)
            .field("last_seen_at", &self.last_seen_at)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// A started login, kept until its callback. `Debug` hides the sealed state.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingLogin {
    /// The login state (`state`, `nonce`, PKCE verifier), sealed with a key derived from the login
    /// cookie: without the browser's cookie it cannot be read.
    pub state: Vec<u8>,
    /// The checked return path.
    pub return_to: String,
    /// End of the login (10 minutes after the start).
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for PendingLogin {
    /// Without the state.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingLogin")
            .field("return_to", &self.return_to)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// The storage of sessions and started logins.
pub trait SessionStore: Send + Sync + 'static {
    /// Stores a new session under `id`.
    fn create(
        &self,
        id: &TokenHash,
        session: &SessionRecord,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// The session stored under `id`, also when it has expired (the caller decides).
    fn load(
        &self,
        id: &TokenHash,
    ) -> impl Future<Output = Result<Option<SessionRecord>, StoreError>> + Send;

    /// Records a use of session `id`: new last use and end.
    fn touch(
        &self,
        id: &TokenHash,
        last_seen_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Deletes session `id`; `true` if it existed.
    fn delete(&self, id: &TokenHash) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// Deletes all sessions with the provider session id `sid`; returns the count.
    fn delete_by_sid(&self, sid: &str) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Deletes all sessions of subject `sub`; returns the count.
    fn delete_by_sub(&self, sub: &str) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Deletes the sessions of subject `sub` that have no `sid`; returns the count. Used when a logout
    /// token names a `sid` no session carries.
    fn delete_by_sub_without_sid(
        &self,
        sub: &str,
    ) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Stores a started login under `id`.
    fn put_login(
        &self,
        id: &TokenHash,
        login: &PendingLogin,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Removes and returns the started login `id` (also when expired). Must be atomic: two concurrent
    /// calls for the same `id` must not both get it.
    fn take_login(
        &self,
        id: &TokenHash,
    ) -> impl Future<Output = Result<Option<PendingLogin>, StoreError>> + Send;
}

/// A boxed future of the type-erased store.
pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'a>>;

/// [`SessionStore`] behind `dyn`, so [`super::Bff`] and the extractor do not carry the store type.
pub(crate) trait DynStore: Send + Sync {
    fn create<'a>(&'a self, id: &'a TokenHash, s: &'a SessionRecord) -> BoxFuture<'a, ()>;
    fn load<'a>(&'a self, id: &'a TokenHash) -> BoxFuture<'a, Option<SessionRecord>>;
    fn touch<'a>(
        &'a self,
        id: &'a TokenHash,
        seen: DateTime<Utc>,
        ends: DateTime<Utc>,
    ) -> BoxFuture<'a, ()>;
    fn delete<'a>(&'a self, id: &'a TokenHash) -> BoxFuture<'a, bool>;
    fn delete_by_sid<'a>(&'a self, sid: &'a str) -> BoxFuture<'a, u64>;
    fn delete_by_sub<'a>(&'a self, sub: &'a str) -> BoxFuture<'a, u64>;
    fn delete_by_sub_without_sid<'a>(&'a self, sub: &'a str) -> BoxFuture<'a, u64>;
    fn put_login<'a>(&'a self, id: &'a TokenHash, l: &'a PendingLogin) -> BoxFuture<'a, ()>;
    fn take_login<'a>(&'a self, id: &'a TokenHash) -> BoxFuture<'a, Option<PendingLogin>>;
}

impl<S: SessionStore> DynStore for S {
    fn create<'a>(&'a self, id: &'a TokenHash, s: &'a SessionRecord) -> BoxFuture<'a, ()> {
        Box::pin(SessionStore::create(self, id, s))
    }
    fn load<'a>(&'a self, id: &'a TokenHash) -> BoxFuture<'a, Option<SessionRecord>> {
        Box::pin(SessionStore::load(self, id))
    }
    fn touch<'a>(
        &'a self,
        id: &'a TokenHash,
        seen: DateTime<Utc>,
        ends: DateTime<Utc>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(SessionStore::touch(self, id, seen, ends))
    }
    fn delete<'a>(&'a self, id: &'a TokenHash) -> BoxFuture<'a, bool> {
        Box::pin(SessionStore::delete(self, id))
    }
    fn delete_by_sid<'a>(&'a self, sid: &'a str) -> BoxFuture<'a, u64> {
        Box::pin(SessionStore::delete_by_sid(self, sid))
    }
    fn delete_by_sub<'a>(&'a self, sub: &'a str) -> BoxFuture<'a, u64> {
        Box::pin(SessionStore::delete_by_sub(self, sub))
    }
    fn delete_by_sub_without_sid<'a>(&'a self, sub: &'a str) -> BoxFuture<'a, u64> {
        Box::pin(SessionStore::delete_by_sub_without_sid(self, sub))
    }
    fn put_login<'a>(&'a self, id: &'a TokenHash, l: &'a PendingLogin) -> BoxFuture<'a, ()> {
        Box::pin(SessionStore::put_login(self, id, l))
    }
    fn take_login<'a>(&'a self, id: &'a TokenHash) -> BoxFuture<'a, Option<PendingLogin>> {
        Box::pin(SessionStore::take_login(self, id))
    }
}

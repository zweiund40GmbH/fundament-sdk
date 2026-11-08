//! An in-memory `SessionStore` for the tests (the crate ships none: sessions belong in shared storage).

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use z40_fundament::StoreError;
use z40_fundament_http::bff::{PendingLogin, SessionRecord, SessionStore, TokenHash};

/// Sessions and logins behind a mutex; clones share the data.
#[derive(Clone, Default)]
pub struct MemoryStore {
    pub sessions: Arc<Mutex<HashMap<TokenHash, SessionRecord>>>,
    pub logins: Arc<Mutex<HashMap<TokenHash, PendingLogin>>>,
}

impl MemoryStore {
    /// Number of stored sessions.
    pub fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }

    /// Changes the stored session `id`.
    pub fn edit(&self, id: &TokenHash, f: impl FnOnce(&mut SessionRecord)) {
        f(self.sessions.lock().unwrap().get_mut(id).unwrap());
    }
}

impl SessionStore for MemoryStore {
    async fn create(&self, id: &TokenHash, s: &SessionRecord) -> Result<(), StoreError> {
        self.sessions.lock().unwrap().insert(*id, s.clone());
        Ok(())
    }
    async fn load(&self, id: &TokenHash) -> Result<Option<SessionRecord>, StoreError> {
        Ok(self.sessions.lock().unwrap().get(id).cloned())
    }
    async fn touch(
        &self,
        id: &TokenHash,
        last_seen_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        if let Some(s) = self.sessions.lock().unwrap().get_mut(id) {
            s.last_seen_at = last_seen_at;
            s.expires_at = expires_at;
        }
        Ok(())
    }
    async fn delete(&self, id: &TokenHash) -> Result<bool, StoreError> {
        Ok(self.sessions.lock().unwrap().remove(id).is_some())
    }
    async fn delete_by_sid(&self, sid: &str) -> Result<u64, StoreError> {
        let mut m = self.sessions.lock().unwrap();
        let before = m.len();
        m.retain(|_, s| s.sid.as_deref() != Some(sid));
        Ok((before - m.len()) as u64)
    }
    async fn delete_by_sub(&self, sub: &str) -> Result<u64, StoreError> {
        let mut m = self.sessions.lock().unwrap();
        let before = m.len();
        m.retain(|_, s| s.sub != sub);
        Ok((before - m.len()) as u64)
    }
    async fn put_login(&self, id: &TokenHash, login: &PendingLogin) -> Result<(), StoreError> {
        self.logins.lock().unwrap().insert(*id, login.clone());
        Ok(())
    }
    async fn take_login(&self, id: &TokenHash) -> Result<Option<PendingLogin>, StoreError> {
        Ok(self.logins.lock().unwrap().remove(id))
    }
}

//! Login and sessions for backend-for-frontend apps (feature `bff`): the browser never sees a token of
//! the login provider. The app's server runs the OIDC login with [`z40_auth`] (Authorization Code with
//! PKCE S256, `state`, `nonce`), keeps its own session and gives the browser only a random session
//! cookie.
//!
//! * [`Bff`] – configuration, store and the app's login hook; [`Bff::router`] serves
//!   `GET <base>/login?returnTo=`, `GET <base>/callback`, `POST <base>/logout` and
//!   `POST <base>/backchannel-logout`
//! * [`Session`] – extractor of the signed-in session (`401` without one); `Option<Session<T>>` for
//!   routes that work either way
//! * [`SessionStore`] – where sessions and started logins live (`bff-mysql`: MariaDB/MySQL)
//! * [`BffConfig`] – `from_lookup` settings, [`return_to`], [`cookies`], [`Timeouts`]
//!
//! **What is stored.** Only the SHA-256 of the session and login cookie values, never the values. The
//! login state (PKCE verifier, nonce) is sealed with AES-256-GCM under a key derived from the login
//! cookie value: a copy of the store does not reveal it. No ID, access or refresh token is kept, so the
//! logout URL carries no `id_token_hint` (the provider may ask to confirm). The session holds `sub`,
//! `sid` and what the app's login hook returns (JSON of the app's type `T`, e.g. name, e-mail, roles).
//!
//! **Rotation.** Every login creates a new random session id (256 bit) and deletes the session of the
//! cookie the browser brought along, so an id planted before the login is never signed in.
//!
//! **CSRF.** `POST <base>/logout` must be covered by the app's [`crate::csrf::Csrf`] layer (the default
//! base `/api/auth` lies under `/api/`); as a second line it refuses a request whose `Sec-Fetch-Site` is
//! present and not `same-origin`. `POST <base>/backchannel-logout` is a server-to-server call without
//! cookies and must be exempt.
//!
//! **Login limit.** Each `GET <base>/login` writes a row before anyone is authenticated, so starts are
//! limited per client address and in total (`<PREFIX>_LOGIN_RATE_PER_IP`, `<PREFIX>_LOGIN_RATE_TOTAL`,
//! default 30 and 300 per minute, counted per process); more get `429` and write nothing. Behind a
//! reverse proxy pass `TrustedProxies::resolver()` to [`BffBuilder::client_ip`] (the default, the TCP
//! peer, warns once when the peer is a loopback or private address). The answer carries
//! `Retry-After: 60`. Residual risk: a flood from many addresses (many /64s) can use up the total and
//! block new logins for everyone until the window ends; existing sessions are unaffected. Counters are
//! per instance, so N instances allow N times the limit. Run the store's cleanup for logins that never
//! came back.
//!
//! **Back-channel logout** ends the sessions of the token's `sid`; if none has it, the sessions of its
//! `sub` without `sid`; a token without `sid` ends all sessions of `sub`. A logout that arrives while a
//! callback of the same login is between its token exchange and storing the session (milliseconds) finds
//! nothing to end, and that session stays until its timeouts; the provider has ended its own session, so
//! the next login there starts afresh.
//!
//! ```no_run
//! use axum::{Json, Router, routing::get};
//! use serde::{Deserialize, Serialize};
//! use z40_fundament_http::bff::{Bff, BffConfig, LoginDenied, Session, SessionStore};
//! use z40_fundament_http::csrf::Csrf;
//!
//! #[derive(Serialize, Deserialize)]
//! struct User { name: String, roles: Vec<String> }
//!
//! async fn me(session: Session<User>) -> Json<Vec<String>> {
//!     Json(session.data.roles)
//! }
//!
//! fn app(store: impl SessionStore) -> Result<Router, Box<dyn std::error::Error>> {
//!     let config = BffConfig::from_lookup("AUTH", |k| std::env::var(k).ok())?;
//!     let bff = Bff::builder(config, store, |client, login| async move {
//!         let access = client
//!             .verify_access_token(login.tokens.access_token.expose_secret(), client.client_id())
//!             .await
//!             .map_err(|_| LoginDenied::unavailable())?;
//!         if access.roles.is_empty() {
//!             return Err(LoginDenied::forbidden("Ihr Konto hat keinen Zugang."));
//!         }
//!         let name = login.claims.name.clone().unwrap_or_default();
//!         Ok(User { name, roles: access.roles })
//!     })
//!     .cookie_name("crm")
//!     .base_path("/api/auth")
//!     .scopes(&["profile", "email", "roles"])
//!     .build()?;
//!     Ok(Router::new()
//!         .route("/api/me", get(me))
//!         .with_state(bff.clone())
//!         .merge(bff.router())
//!         .layer(Csrf::header("x-requested-with", "crm")?.exempt_path("/api/auth/backchannel-logout")))
//! }
//! ```

mod builder;
pub mod config;
pub mod cookies;
mod limit;
mod logout;
mod page;
pub mod return_to;
mod routes;
mod session;
mod store;
pub mod timeouts;
pub mod token;

pub use builder::BffBuilder;
pub use config::{BffConfig, InvalidUrl};
pub use cookies::SameSite;
pub use limit::ClientIpFn;
pub use page::{LoginDenied, LoginFailure};
pub use session::{Session, SessionRejection};
pub use store::{PendingLogin, SessionRecord, SessionStore};
pub use timeouts::Timeouts;
pub use token::TokenHash;

use axum::response::Response;
use cookies::Cookies;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use store::DynStore;
use tokio::sync::OnceCell;
use z40_auth::{Client, ClientSecret, LoginResult, Options};

/// What a session holds for the app: any serializable type (stored as JSON).
pub trait SessionData:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
}
impl<T: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static> SessionData for T {}

/// The future of a login hook.
pub type LoginFuture<T> = Pin<Box<dyn Future<Output = Result<T, LoginDenied>> + Send>>;
/// The app's login hook: from the verified login (claims and tokens) and the client (e.g. to verify the
/// access token for roles) to the session data, or a refusal.
type LoginHook<T> = Arc<dyn Fn(Arc<Client>, LoginResult) -> LoginFuture<T> + Send + Sync>;
/// Renders the page of a failed login.
type ErrorPage = Arc<dyn Fn(&LoginFailure) -> Response + Send + Sync>;

/// A failure of the session machinery. Messages carry no values.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BffError {
    /// The session store failed.
    #[error("session store failed")]
    Store(#[from] z40_fundament::StoreError),
    /// The login provider could not be reached (discovery failed).
    #[error("login provider not reachable")]
    Unavailable,
    /// Randomness or encryption failed.
    #[error(transparent)]
    Crypto(#[from] token::CryptoError),
    /// The session data could not be serialized or is larger than [`DATA_MAX`].
    #[error("session data could not be stored")]
    Data,
}

/// A setting of [`BffBuilder`] is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BffBuildError {
    /// See [`cookies::InvalidCookieName`].
    #[error(transparent)]
    CookieName(#[from] cookies::InvalidCookieName),
    /// The base path is not `/` followed by segments of `a-z A-Z 0-9 _ - .`, without a trailing `/`.
    #[error("invalid base path")]
    BasePath,
    /// A scope is empty or contains characters outside visible ASCII (or `"`, `\`).
    #[error("invalid scope")]
    Scope,
    /// A URL of the configuration breaks the rules of [`config`].
    #[error(transparent)]
    Url(#[from] InvalidUrl),
    /// A login limit of 0.
    #[error("login limit must be at least 1")]
    LoginLimit,
}

/// Longest accepted `sub` (and the `sub` column of `bff-mysql`).
pub const SUB_MAX: usize = 255;
/// Longest accepted `sid`.
pub const SID_MAX: usize = 255;
/// Largest session data (JSON bytes).
pub const DATA_MAX: usize = 16 * 1024;

/// Login and sessions of an app; cheap to clone (put it in the app state and provide it via
/// `FromRef` for [`Session`]). `T` is the app's session data.
pub struct Bff<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Clone for Bff<T> {
    /// Shares the same instance.
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> std::fmt::Debug for Bff<T> {
    /// Configuration (secret redacted) and paths.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bff")
            .field("config", &self.inner.config)
            .field("base", &self.inner.base)
            .finish_non_exhaustive()
    }
}

/// The shared parts of [`Bff`].
struct Inner<T> {
    config: BffConfig,
    store: Box<dyn DynStore>,
    cookies: Cookies,
    base: String,
    scopes: Vec<String>,
    on_login: LoginHook<T>,
    error_page: ErrorPage,
    options: Options,
    client: OnceCell<Arc<Client>>,
    limit: Option<limit::LoginLimit>,
}

impl<T: SessionData> Bff<T> {
    /// The settings.
    pub fn config(&self) -> &BffConfig {
        &self.inner.config
    }

    /// Names and attributes of the cookies.
    pub fn cookies(&self) -> &Cookies {
        &self.inner.cookies
    }

    /// The discovered client of the login provider. Discovery happens on first use and is retried until
    /// it succeeds, so the app starts while the provider is unreachable.
    pub async fn client(&self) -> Result<Arc<Client>, BffError> {
        let i = &self.inner;
        i.client
            .get_or_try_init(|| async {
                let secret = ClientSecret::new(i.config.client_secret.expose());
                let options = i.options.clone();
                Client::discover_with(&i.config.issuer, &i.config.client_id, secret, options)
                    .await
                    .map(Arc::new)
            })
            .await
            .cloned()
            .map_err(|e| {
                tracing::warn!(error = %e, "discovery of the login provider failed");
                BffError::Unavailable
            })
    }

    /// Ends all sessions of subject `sub` (e.g. when the app locks an account); returns the count.
    pub async fn end_sessions_of(&self, sub: &str) -> Result<u64, BffError> {
        Ok(self.inner.store.delete_by_sub(sub).await?)
    }
}

//! [`BffBuilder`]: the settings of a [`Bff`] besides the configuration, checked in
//! [`BffBuilder::build`].

use super::cookies::Cookies;
use super::limit::{ClientIpFn, DynLimitStore, LoginLimit, LoginLimitStore, SharedCounters};
use super::page;
use super::store::DynStore;
use super::{
    Bff, BffBuildError, BffConfig, ErrorPage, Inner, LoginDenied, LoginFailure, LoginHook,
    SessionData, SessionStore,
};
use axum::http::request::Parts;
use axum::response::Response;
use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::OnceCell;
use z40_auth::{Client, LoginResult, Options};

/// Builder of [`Bff`] (from [`Bff::builder`]).
pub struct BffBuilder<T> {
    config: BffConfig,
    store: Box<dyn DynStore>,
    on_login: LoginHook<T>,
    cookie_name: String,
    base: String,
    scopes: Vec<String>,
    error_page: ErrorPage,
    options: Options,
    limit: Option<(u32, u32)>,
    client_ip: Option<ClientIpFn>,
    limit_store: Option<Box<dyn DynLimitStore>>,
}

impl<T> std::fmt::Debug for BffBuilder<T> {
    /// Configuration (secret redacted) and paths.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BffBuilder")
            .field("config", &self.config)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl<T: SessionData> Bff<T> {
    /// Starts a builder with the settings, the session store and the login hook. The hook runs after the
    /// login was verified and returns the session data, or a [`LoginDenied`] (e.g. no role for this
    /// app); it gets the client to verify the access token if it needs roles.
    pub fn builder<S, F, Fut>(config: BffConfig, store: S, on_login: F) -> BffBuilder<T>
    where
        S: SessionStore,
        F: Fn(Arc<Client>, LoginResult) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T, LoginDenied>> + Send + 'static,
    {
        let limit = Some((config.login_rate_per_ip, config.login_rate_total));
        BffBuilder {
            config,
            store: Box::new(store),
            on_login: Arc::new(move |c, l| Box::pin(on_login(c, l))),
            cookie_name: "app".to_owned(),
            base: "/api/auth".to_owned(),
            scopes: vec!["profile".to_owned(), "email".to_owned()],
            limit,
            client_ip: None,
            limit_store: None,
            error_page: Arc::new(page::default_page),
            options: Options::default(),
        }
    }
}

impl<T: SessionData> BffBuilder<T> {
    /// Base name of the cookies: `__Host-<name>_sid` and `__Host-<name>_login` (default `app`).
    pub fn cookie_name(mut self, name: &str) -> Self {
        name.clone_into(&mut self.cookie_name);
        self
    }

    /// Path prefix of the routes (default `/api/auth`, so a `Csrf` layer on `/api/` covers the logout): `<base>/login`, `<base>/callback`, … . The redirect
    /// URI of the configuration must end in `<base>/callback` as the browser sees it.
    pub fn base_path(mut self, base: &str) -> Self {
        base.clone_into(&mut self.base);
        self
    }

    /// Scopes of the login besides `openid` (default `profile`, `email`). Add `roles` for app roles in
    /// the access token. No `offline_access`: this crate keeps no refresh tokens.
    pub fn scopes(mut self, scopes: &[&str]) -> Self {
        self.scopes = scopes.iter().map(|s| (*s).to_owned()).collect();
        self
    }

    /// Limits login starts to `per_ip` per client address and `total` in all per minute (default: from
    /// the configuration, 30 and 300). Both at least 1.
    pub fn login_limit(mut self, per_ip: u32, total: u32) -> Self {
        self.limit = Some((per_ip, total));
        self
    }

    /// Turns the login limit off, e.g. when the app limits `GET <base>/login` itself.
    pub fn without_login_limit(mut self) -> Self {
        self.limit = None;
        self
    }

    /// Keeps the counters of the login limit in `store`, shared by all instances of the app (e.g.
    /// `bff_mysql::MySqlLoginLimitStore`), instead of in this process: N instances then allow the
    /// configured limit, not N times it. Same rules (per client address, IPv6 by /64, and in total;
    /// `429` with `Retry-After`, nothing written); windows are full minutes (UTC). Addresses are stored
    /// as a keyed hash (HMAC under a key derived from the client secret). When the store fails,
    /// the login start fails (`500` page) instead of passing unlimited.
    pub fn login_limit_store(mut self, store: impl LoginLimitStore) -> Self {
        self.limit_store = Some(Box::new(store));
        self
    }

    /// How the login limit finds the client address (default: the TCP peer from
    /// `ConnectInfo<SocketAddr>`). Behind a reverse proxy pass `TrustedProxies::resolver()`. Without an
    /// address all clients share one counter (warned once).
    pub fn client_ip(
        mut self,
        resolve: impl Fn(&Parts) -> Option<IpAddr> + Send + Sync + 'static,
    ) -> Self {
        self.client_ip = Some(Arc::new(resolve));
        self
    }

    /// Options of the login client (HTTP client, JWKS cache); default [`Options::default`].
    pub fn auth_options(mut self, options: Options) -> Self {
        self.options = options;
        self
    }

    /// Renders failed logins with `page` instead of the built-in German HTML page. Messages of
    /// [`LoginFailure`] are fixed texts, never input.
    pub fn error_page(
        mut self,
        page: impl Fn(&LoginFailure) -> Response + Send + Sync + 'static,
    ) -> Self {
        self.error_page = Arc::new(page);
        self
    }

    /// Checks the settings and builds the [`Bff`].
    pub fn build(self) -> Result<Bff<T>, BffBuildError> {
        self.config.check()?;
        let cookies = Cookies::new(
            &self.cookie_name,
            self.config.same_site,
            self.config.secure_cookies(),
        )?;
        if !valid_base(&self.base) {
            return Err(BffBuildError::BasePath);
        }
        let scope_ok = |s: &String| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\')
        };
        if !self.scopes.iter().all(scope_ok) {
            return Err(BffBuildError::Scope);
        }
        let limit = match self.limit {
            Some((per_ip, total)) if per_ip == 0 || total == 0 => {
                return Err(BffBuildError::LoginLimit);
            }
            Some((per_ip, total)) => {
                let secret = &self.config.client_secret;
                let shared = self
                    .limit_store
                    .map(|store| SharedCounters::new(store, secret))
                    .transpose()?;
                Some(LoginLimit::new(per_ip, total, self.client_ip, shared))
            }
            None => None,
        };
        Ok(Bff {
            inner: Arc::new(Inner {
                config: self.config,
                store: self.store,
                cookies,
                base: self.base,
                scopes: self.scopes,
                on_login: self.on_login,
                error_page: self.error_page,
                options: self.options,
                client: OnceCell::new(),
                limit,
            }),
        })
    }
}

/// `/` followed by non-empty segments of `a-z A-Z 0-9 _ - .`, no trailing `/` (also no route syntax).
fn valid_base(base: &str) -> bool {
    base.strip_prefix('/').is_some_and(|rest| {
        rest.split('/').all(|seg| {
            !seg.is_empty()
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::valid_base;

    #[test]
    fn base_paths() {
        for ok in ["/auth", "/api/auth", "/a.b/c-d_e"] {
            assert!(valid_base(ok), "{ok}");
        }
        for bad in [
            "", "/", "auth", "/auth/", "//auth", "/a//b", "/{x}", "/*x", "/a b", "/ä",
        ] {
            assert!(!valid_base(bad), "{bad}");
        }
    }
}

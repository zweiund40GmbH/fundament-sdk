//! The app of the `bff` tests: a `Bff` against the fake provider with the in-memory store, a route
//! `/api/me` behind the `Session` extractor, and a browser-like client (cookies by hand).

use super::memory::MemoryStore;
use super::oidc::{CLIENT_ID, Provider};
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower::ServiceExt;
use z40_fundament_http::bff::{
    Bff, BffBuilder, BffConfig, Defaults, LoginDenied, Session, SessionStore,
};

/// The app's session data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub name: String,
    pub roles: Vec<String>,
}

/// Text of the hook's refusal.
pub const NO_ROLE: &str = "Ihr Konto hat keinen Zugang.";

/// The test app over the store `S`.
pub struct TestApp<S = MemoryStore> {
    pub provider: Provider,
    pub store: S,
    pub bff: Bff<User>,
    pub router: Router,
}

async fn me(session: Session<User>) -> Json<User> {
    Json(session.data)
}

async fn maybe(session: Option<Session<User>>) -> &'static str {
    if session.is_some() { "yes" } else { "no" }
}

impl TestApp {
    /// Provider, in-memory store and app; `vars` overrides settings (`AUTH_…`).
    pub async fn start(vars: &[(&str, &str)]) -> Self {
        Self::with_store(MemoryStore::default(), vars).await
    }
}

impl<S: SessionStore + Clone> TestApp<S> {
    /// Provider and app over `store`; `vars` overrides settings (`AUTH_…`).
    pub async fn with_store(store: S, vars: &[(&str, &str)]) -> Self {
        Self::custom(store, vars, |b| b).await
    }

    /// Like [`TestApp::with_store`], with `customize` applied to the builder. The client address comes
    /// from the header `x-test-ip` (none without it).
    pub async fn custom(
        store: S,
        vars: &[(&str, &str)],
        customize: impl FnOnce(BffBuilder<User>) -> BffBuilder<User>,
    ) -> Self {
        let provider = Provider::start().await;
        let mut env: Vec<(String, String)> = vec![
            ("AUTH_ISSUER".into(), provider.issuer.clone()),
            ("AUTH_CLIENT_ID".into(), CLIENT_ID.into()),
            ("AUTH_CLIENT_SECRET".into(), "s3cr3t".into()),
            (
                "AUTH_REDIRECT_URI".into(),
                "https://crm.example.com/api/auth/callback".into(),
            ),
            (
                "AUTH_POST_LOGOUT_REDIRECT_URI".into(),
                "https://crm.example.com/".into(),
            ),
        ];
        env.extend(vars.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
        let lookup = move |k: &str| {
            env.iter()
                .rev()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
        };
        // The fake provider runs on plain HTTP at 127.0.0.1, as in local development.
        let dev = Defaults {
            allow_loopback_http: true,
            ..Defaults::default()
        };
        let config = BffConfig::from_lookup_with("AUTH", lookup, dev).unwrap();
        let bff = Bff::builder(config, store.clone(), |client, login| async move {
            let access = client
                .verify_access_token(
                    login.tokens.access_token.expose_secret(),
                    client.client_id(),
                )
                .await
                .map_err(|_| LoginDenied::unavailable())?;
            if access.roles.is_empty() {
                return Err(LoginDenied::forbidden(NO_ROLE));
            }
            Ok(User {
                name: login.claims.name.clone().unwrap_or_default(),
                roles: access.roles,
            })
        })
        .cookie_name("crm")
        .scopes(&["profile", "email", "roles"])
        .client_ip(|parts| {
            let ip = parts.headers.get("x-test-ip")?.to_str().ok()?;
            ip.parse().ok()
        });
        let bff = customize(bff).build().unwrap();
        let router = Router::new()
            .route("/api/me", get(me))
            .route("/api/maybe", get(maybe))
            .with_state(bff.clone())
            .merge(bff.router());
        Self {
            provider,
            store,
            bff,
            router,
        }
    }

    /// Sends a request with `cookies` (a `Cookie` header value) and extra headers.
    pub async fn send(
        &self,
        method: Method,
        uri: &str,
        cookies: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Response {
        let mut b = Request::builder().method(method).uri(uri);
        if !cookies.is_empty() {
            b = b.header(header::COOKIE, cookies);
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let req = b.body(Body::from(body.to_owned())).unwrap();
        self.router.clone().oneshot(req).await.unwrap()
    }

    /// `GET /api/auth/login?returnTo=…`: the authorize URL and the login cookie (`name=value`).
    pub async fn start_login(&self, return_to: &str) -> (String, String) {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("returnTo", return_to)
            .finish();
        let res = self
            .send(Method::GET, &format!("/api/auth/login?{q}"), "", &[], "")
            .await;
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        let cookie = set_cookies(res.headers())
            .into_iter()
            .find(|c| c.starts_with("__Host-crm_login="))
            .unwrap();
        (location(&res), name_value(&cookie))
    }

    /// A full login of `sub` (with `sid`) and roles; returns the session cookie (`name=value`).
    pub async fn login_as(&self, sub: &str, sid: Option<&str>, existing: &str) -> String {
        let (authorize, login_cookie) = self.start_login("/").await;
        let a = self
            .provider
            .authorize(&authorize, sub, sid, &["admin"], |_| {});
        let cookies = [login_cookie.as_str(), existing]
            .iter()
            .filter(|c| !c.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("; ");
        let res = self
            .send(
                Method::GET,
                &format!("/api/auth/callback?{}", a.query),
                &cookies,
                &[],
                "",
            )
            .await;
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        session_cookie(res.headers()).unwrap()
    }
}

/// All `Set-Cookie` values.
pub fn set_cookies(h: &HeaderMap) -> Vec<String> {
    h.get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .collect()
}

/// The new session cookie (`name=value`), if one with a value was set.
pub fn session_cookie(h: &HeaderMap) -> Option<String> {
    set_cookies(h)
        .into_iter()
        .find(|c| c.starts_with("__Host-crm_sid=") && !c.starts_with("__Host-crm_sid=;"))
        .map(|c| name_value(&c))
}

/// `name=value` of a `Set-Cookie` value.
pub fn name_value(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap().to_owned()
}

/// The `Location` header.
pub fn location(res: &Response) -> String {
    res.headers()[header::LOCATION].to_str().unwrap().to_owned()
}

/// The body as text.
pub async fn text(res: Response) -> String {
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

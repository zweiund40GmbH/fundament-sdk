//! [`FakeProvider`]: discovery, JWKS with one ES256 key and a token endpoint that checks the PKCE
//! verifier. Panics instead of returning errors where a test can only fail anyway (documented per
//! function).

use aws_lc_rs::digest::{SHA256, digest};
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use axum::extract::{Form, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use url::Url;

/// A registered authorization code.
struct Grant {
    challenge: String,
    redirect_uri: String,
    id_claims: Value,
    access_claims: Value,
}

/// A running fake login provider for one client id (see the [module docs](super)). Cheap to clone;
/// it serves until the runtime ends.
#[derive(Clone)]
pub struct FakeProvider {
    issuer: String,
    client_id: String,
    key: Arc<EcdsaKeyPair>,
    grants: Arc<Mutex<HashMap<String, Grant>>>,
}

impl std::fmt::Debug for FakeProvider {
    /// Issuer and client id.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeProvider")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// What [`FakeProvider::authorize`] read from the authorize URL and issued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorized {
    /// The callback query the browser would bring back (`code`, `state`, `iss`).
    pub query: String,
    /// `state` of the request.
    pub state: String,
}

/// Seconds since the Unix epoch.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The value of query parameter `name` of the authorize URL.
fn param<'a>(q: &'a HashMap<String, String>, name: &str) -> &'a str {
    match q.get(name) {
        Some(v) => v,
        None => panic!("authorize URL without `{name}`"),
    }
}

impl FakeProvider {
    /// Starts a provider for `client_id` on a free port of `127.0.0.1` (a task of the current tokio
    /// runtime). Fails when no port can be bound or no key generated.
    pub async fn start(client_id: &str) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let issuer = format!("http://{}", listener.local_addr()?);
        let key = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING)
            .map_err(|_| std::io::Error::other("key generation failed"))?;
        let p = Self {
            issuer,
            client_id: client_id.to_owned(),
            key: Arc::new(key),
            grants: Arc::default(),
        };
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .with_state(p.clone());
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::warn!(error = %e, "fake login provider stopped");
            }
        });
        Ok(p)
    }

    /// The issuer (`http://127.0.0.1:<port>`), also the base of its endpoints.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The client id it issues tokens for.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The registered codes, also after a panic of another test thread.
    fn grants(&self) -> MutexGuard<'_, HashMap<String, Grant>> {
        self.grants.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A JWT with header `typ` (`JWT`, `at+jwt`, `logout+jwt`, …) and `claims`, signed with the
    /// provider's key (ES256, `kid` `k1`), e.g. to forge tokens for negative tests.
    ///
    /// # Panics
    ///
    /// If signing fails (no randomness).
    pub fn sign(&self, typ: &str, claims: &Value) -> String {
        let header = json!({"alg": "ES256", "typ": typ, "kid": "k1"});
        let input = format!(
            "{}.{}",
            B64.encode(header.to_string()),
            B64.encode(claims.to_string())
        );
        match self.key.sign(&SystemRandom::new(), input.as_bytes()) {
            Ok(sig) => format!("{input}.{}", B64.encode(sig.as_ref())),
            Err(_) => panic!("signing the test token failed"),
        }
    }

    /// Registers a code for the authorize URL `authorize` of the app's redirect. The ID token gets
    /// `sub`, `sid`, the request's `nonce` and a name; `tweak` may change its claims before signing
    /// (e.g. a wrong nonce). The access token carries `roles`.
    ///
    /// # Panics
    ///
    /// If `authorize` is not this provider's authorize URL for its client id with a code request using
    /// PKCE S256 (the test found a broken redirect).
    pub fn authorize(
        &self,
        authorize: &str,
        sub: &str,
        sid: Option<&str>,
        roles: &[&str],
        tweak: impl FnOnce(&mut Value),
    ) -> Authorized {
        let Ok(url) = Url::parse(authorize) else {
            panic!("authorize URL is not a URL");
        };
        assert!(
            authorize.starts_with(&format!("{}/authorize?", self.issuer)),
            "not the authorize URL of this provider"
        );
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(param(&q, "response_type"), "code");
        assert_eq!(param(&q, "client_id"), self.client_id);
        assert_eq!(param(&q, "code_challenge_method"), "S256");
        let state = param(&q, "state");
        let t = now();
        let mut id_claims = json!({
            "iss": self.issuer, "sub": sub, "aud": self.client_id, "exp": t + 300, "iat": t,
            "nonce": param(&q, "nonce"), "env": "test", "name": "Erika Mustermann", "email": "erika@example.com",
        });
        if let Some(sid) = sid {
            id_claims["sid"] = json!(sid);
        }
        tweak(&mut id_claims);
        let access_claims = json!({
            "iss": self.issuer, "sub": sub, "aud": self.client_id, "client_id": self.client_id,
            "scope": q.get("scope"), "exp": t + 300, "iat": t, "jti": format!("at-{t}-{sub}"),
            "env": "test", "roles": roles,
        });
        let code = format!("code-{}", B64.encode(digest(&SHA256, state.as_bytes())));
        self.grants().insert(
            code.clone(),
            Grant {
                challenge: param(&q, "code_challenge").to_owned(),
                redirect_uri: param(&q, "redirect_uri").to_owned(),
                id_claims,
                access_claims,
            },
        );
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("code", &code)
            .append_pair("state", state)
            .append_pair("iss", &self.issuer)
            .finish();
        Authorized {
            query,
            state: state.to_owned(),
        }
    }

    /// A signed back-channel logout token for `sub` and/or `sid`.
    pub fn logout_token(&self, sub: Option<&str>, sid: Option<&str>) -> String {
        let t = now();
        let mut claims = json!({
            "iss": self.issuer, "aud": self.client_id, "iat": t, "exp": t + 120, "jti": format!("lt-{t}"),
            "events": {"http://schemas.openid.net/event/backchannel-logout": {}},
        });
        if let Some(sub) = sub {
            claims["sub"] = json!(sub);
        }
        if let Some(sid) = sid {
            claims["sid"] = json!(sid);
        }
        self.sign("logout+jwt", &claims)
    }
}

/// The discovery document.
async fn discovery(State(p): State<FakeProvider>) -> Json<Value> {
    let i = &p.issuer;
    Json(json!({
        "issuer": i,
        "authorization_endpoint": format!("{i}/authorize"),
        "token_endpoint": format!("{i}/token"),
        "jwks_uri": format!("{i}/jwks"),
        "end_session_endpoint": format!("{i}/logout"),
        "id_token_signing_alg_values_supported": ["ES256"],
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
    }))
}

/// The key set with the provider's public key.
async fn jwks(State(p): State<FakeProvider>) -> Json<Value> {
    // An uncompressed P-256 point: 0x04, x (32 bytes), y (32 bytes).
    let point = p.key.public_key().as_ref();
    assert_eq!(point.len(), 65, "uncompressed P-256 point");
    Json(json!({"keys": [{
        "kty": "EC", "crv": "P-256", "kid": "k1", "alg": "ES256", "use": "sig",
        "x": B64.encode(&point[1..33]), "y": B64.encode(&point[33..65]),
    }]}))
}

/// The code exchange: single-use code, PKCE S256 and the redirect URI are checked.
async fn token(
    State(p): State<FakeProvider>,
    Form(f): Form<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let bad = || {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant"})),
        )
    };
    if f.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return Err(bad());
    }
    let code = f.get("code").ok_or_else(bad)?;
    let grant = p.grants().remove(code).ok_or_else(bad)?;
    let verifier = f.get("code_verifier").ok_or_else(bad)?;
    if B64.encode(digest(&SHA256, verifier.as_bytes())) != grant.challenge
        || f.get("redirect_uri") != Some(&grant.redirect_uri)
    {
        return Err(bad());
    }
    Ok(Json(json!({
        "access_token": p.sign("at+jwt", &grant.access_claims),
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": p.sign("JWT", &grant.id_claims),
    })))
}

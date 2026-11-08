//! A fake login provider on 127.0.0.1 for the `bff` tests: discovery, JWKS with one ES256 key and a
//! token endpoint that checks the PKCE verifier. The test plays the browser: it reads the authorize URL
//! of the app's redirect and registers a code with [`Provider::authorize`].

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
use std::sync::{Arc, Mutex};
use url::Url;

/// Client id the provider knows.
pub const CLIENT_ID: &str = "crm";

/// A registered authorization code.
struct Grant {
    challenge: String,
    redirect_uri: String,
    id_claims: Value,
    access_claims: Value,
}

/// The running provider.
#[derive(Clone)]
pub struct Provider {
    pub issuer: String,
    key: Arc<EcdsaKeyPair>,
    grants: Arc<Mutex<HashMap<String, Grant>>>,
}

/// What [`Provider::authorize`] read from the authorize URL and issued.
pub struct Authorized {
    /// The callback query the browser would bring back (`code`, `state`, `iss`).
    pub query: String,
    /// `state` of the request.
    pub state: String,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

impl Provider {
    /// Starts the provider.
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let key = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_FIXED_SIGNING).unwrap();
        let p = Self {
            issuer,
            key: Arc::new(key),
            grants: Arc::default(),
        };
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .with_state(p.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        p
    }

    /// A signed JWT with header `typ`.
    pub fn sign(&self, typ: &str, claims: &Value) -> String {
        let header = json!({"alg": "ES256", "typ": typ, "kid": "k1"});
        let input = format!(
            "{}.{}",
            B64.encode(header.to_string()),
            B64.encode(claims.to_string())
        );
        let sig = self
            .key
            .sign(&SystemRandom::new(), input.as_bytes())
            .unwrap();
        format!("{input}.{}", B64.encode(sig.as_ref()))
    }

    /// Registers a code for the authorize URL `authorize` of the app's redirect. The ID token gets
    /// `sub`, `sid`, the request's `nonce` and a name; `tweak` may change its claims before signing
    /// (e.g. a wrong nonce). The access token carries `roles`.
    pub fn authorize(
        &self,
        authorize: &str,
        sub: &str,
        sid: Option<&str>,
        roles: &[&str],
        tweak: impl FnOnce(&mut Value),
    ) -> Authorized {
        let url = Url::parse(authorize).unwrap();
        assert!(authorize.starts_with(&format!("{}/authorize?", self.issuer)));
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], CLIENT_ID);
        assert_eq!(q["code_challenge_method"], "S256");
        let t = now();
        let mut id_claims = json!({
            "iss": self.issuer, "sub": sub, "aud": CLIENT_ID, "exp": t + 300, "iat": t,
            "nonce": q["nonce"], "env": "test", "name": "Erika Mustermann", "email": "erika@example.com",
        });
        if let Some(sid) = sid {
            id_claims["sid"] = json!(sid);
        }
        tweak(&mut id_claims);
        let access_claims = json!({
            "iss": self.issuer, "sub": sub, "aud": CLIENT_ID, "client_id": CLIENT_ID,
            "scope": q["scope"], "exp": t + 300, "iat": t, "jti": format!("at-{t}-{sub}"),
            "env": "test", "roles": roles,
        });
        let code = format!(
            "code-{}",
            B64.encode(digest(&SHA256, q["state"].as_bytes()))
        );
        self.grants.lock().unwrap().insert(
            code.clone(),
            Grant {
                challenge: q["code_challenge"].clone(),
                redirect_uri: q["redirect_uri"].clone(),
                id_claims,
                access_claims,
            },
        );
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("code", &code)
            .append_pair("state", &q["state"])
            .append_pair("iss", &self.issuer)
            .finish();
        Authorized {
            query,
            state: q["state"].clone(),
        }
    }

    /// A back-channel logout token for `sub` and/or `sid`.
    pub fn logout_token(&self, sub: Option<&str>, sid: Option<&str>) -> String {
        let t = now();
        let mut claims = json!({
            "iss": self.issuer, "aud": CLIENT_ID, "iat": t, "exp": t + 120, "jti": format!("lt-{t}"),
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

async fn discovery(State(p): State<Provider>) -> Json<Value> {
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

async fn jwks(State(p): State<Provider>) -> Json<Value> {
    let point = p.key.public_key().as_ref();
    assert_eq!(point.len(), 65, "uncompressed P-256 point");
    Json(json!({"keys": [{
        "kty": "EC", "crv": "P-256", "kid": "k1", "alg": "ES256", "use": "sig",
        "x": B64.encode(&point[1..33]), "y": B64.encode(&point[33..65]),
    }]}))
}

/// The code exchange: single-use code, PKCE S256 and the redirect URI are checked.
async fn token(
    State(p): State<Provider>,
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
    let grant = p
        .grants
        .lock()
        .unwrap()
        .remove(f.get("code").ok_or_else(bad)?)
        .ok_or_else(bad)?;
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

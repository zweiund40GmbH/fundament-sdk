# z40-fundament-http

[![crates.io](https://img.shields.io/crates/v/z40-fundament-http.svg)](https://crates.io/crates/z40-fundament-http)
[![docs.rs](https://img.shields.io/docsrs/z40-fundament-http)](https://docs.rs/z40-fundament-http)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

HTTP groundwork for [axum](https://crates.io/crates/axum) 0.8 apps – the parts every app otherwise copies:
request ids, a trace layer that never logs secrets, the global log setup, security headers, `no-store`, the
client address behind trusted proxies, a CSRF header guard and the server-side OIDC login with sessions for
backend-for-frontend apps. Part of
[fundament-sdk](https://github.com/zweiund40GmbH/fundament-sdk); settings are read with the `from_lookup`
helpers of [`z40-fundament`](https://crates.io/crates/z40-fundament).

**Status:** pre-release (`0.x`). The API may still change before `1.0`.

## Features

| Feature | What it gives you |
|---|---|
| *(none)* | `SecurityHeaders` (CSP checked: no unsafe script sources (`'unsafe-inline'`, `'unsafe-eval'`, `'unsafe-hashes'`, `*`, bare schemes), `frame-ancestors` required and not open; `X-Frame-Options`, `nosniff`, `Referrer-Policy`, `Permissions-Policy`, `Cross-Origin-*`; no HSTS), `NoStore`, `TrustedProxies` / `ClientIp` (`X-Forwarded-For` only from trusted proxies), `Csrf` (custom header on state-changing requests, exempt paths) |
| `request-id` | `RequestIdLayer`: keeps a safe incoming `X-Request-Id` (≤ 64 of `[A-Za-z0-9._:-]`) or creates a UUID v7, echoes it, `RequestId` extension |
| `trace` | `trace::layer()`: span with method, route template (`<unmatched>` for unknown routes, raw path only on opt-in) and request id; status and latency at the end; never query strings, headers or bodies |
| `bff` | Login and sessions for backend-for-frontend apps with [`z40-auth`](https://crates.io/crates/z40-auth): `Bff::router()` (`GET <base>/login?returnTo=`, `GET <base>/callback`, `POST <base>/logout`, `POST <base>/backchannel-logout`), `Session<T>` extractor, `SessionStore` trait, `BffConfig` (`<PREFIX>_ISSUER`, `<PREFIX>_CLIENT_ID`, …) |
| `bff-mysql` | `MySqlSessionStore` (MariaDB/MySQL, sqlx) with table prefix, `schema/mysql-session.sql` and `delete_expired` for the worker |
| `telemetry` | `TelemetryConfig`: JSON lines or one-line text, `EnvFilter`, from `<PREFIX>_LOG` (unset: `RUST_LOG`, then `info`) and `<PREFIX>_LOG_FORMAT` (`json` \| `text`); `from_lookup_with` takes the app's defaults (filter, format); `init` reports what it did (installed, installed without `log` bridge, already installed) |

## Quick start

```toml
[dependencies]
z40-fundament-http = { version = "0.5", features = ["request-id", "trace", "telemetry"] }
```

```rust,ignore
use z40_fundament_http::client_ip::TrustedProxies;
use z40_fundament_http::csrf::Csrf;
use z40_fundament_http::no_store::NoStore;
use z40_fundament_http::request_id::RequestIdLayer;
use z40_fundament_http::security_headers::SecurityHeaders;
use z40_fundament_http::telemetry::TelemetryConfig;
use z40_fundament_http::trace;

let env = |k: &str| std::env::var(k).ok();
TelemetryConfig::from_lookup("APP", env)?.init()?;      // APP_LOG (else RUST_LOG, else info), APP_LOG_FORMAT
let proxies = TrustedProxies::from_lookup("APP", env)?; // APP_TRUSTED_PROXIES

// The `ClientIp` extractor takes the proxies from the router state (`AppState` holds `proxies`):
impl axum::extract::FromRef<AppState> for TrustedProxies { fn from_ref(s: &AppState) -> Self { s.proxies.clone() } }

// The last `layer` call runs first: request id → trace → headers → no-store → CSRF → routes.
let app = routes
    .layer(Csrf::header("x-requested-with", "crm")?.protect_prefix("/api/").exempt_path("/api/webhook"))
    .layer(NoStore::new().prefix("/api/").prefix("/fundament/"))
    .layer(SecurityHeaders::api())
    .layer(trace::layer())
    .layer(RequestIdLayer);
axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;
```

- **Client address:** `ClientIp` is an extractor (`TrustedProxies` in the state via the `FromRef` impl above);
  `proxies.resolver()` is the `client_ip` function for `StatusEndpoint` and `FeedEndpoint` of
  `z40-fundament`, so their failure brake sees the same address. Router tests without a server add
  `.layer(axum::extract::connect_info::MockConnectInfo(addr))`; both honour it.
- **CSRF answer:** `403 {"code":"CSRF","message":"Anfrage abgelehnt (CSRF-Schutz). Bitte laden Sie die Seite
  neu."}` – the error form and German fixed texts of the `z40-fundament` endpoints. Apps with their own error
  shape pass `.rejection(|| …)`; apps with another check (e.g. a double-submit cookie) use `Csrf::check(…)`.
- **CSRF header without a start-up error:** `Csrf::header_static(const { CsrfHeader::new("x-requested-with",
  "crm") })` checks name and value at compile time (a safelisted name or an empty value does not compile), so
  no fallback for a failed `Csrf::header(…)?` is needed.
- **CSP per route group:** headers are only added when missing. A route that sets its own CSP keeps it (`X-Frame-Options` then follows its `frame-ancestors`); a
  router group gets its own `SecurityHeaders` layer before it is merged.
- **HSTS** is the job of the edge proxy that terminates TLS.
- **Log defaults of the app:** `TelemetryConfig::from_lookup_with("APP", env, Defaults { filter:
  "info,sqlx=warn".into(), format: if dev { LogFormat::Text } else { LogFormat::Json } })` – used when
  `APP_LOG` (and `RUST_LOG`) or `APP_LOG_FORMAT` are unset, so the app needs no wrapper.

## Login and sessions (`bff`)

The browser never holds a token of the login provider: the server runs the login (Authorization Code with
PKCE S256, `state`, `nonce`, RFC 9207 `iss`, all checked by `z40-auth`) and gives the browser a random
session cookie.

```toml
[dependencies]
z40-fundament-http = { version = "0.5", features = ["bff-mysql"] }
```

```rust,ignore
use z40_fundament_http::bff::{Bff, BffConfig, LoginDenied, Session};
use z40_fundament_http::bff_mysql::{MySqlSessionStore, SessionTables};

#[derive(serde::Serialize, serde::Deserialize)]
struct User { name: String, roles: Vec<String> }

let config = BffConfig::from_lookup("AUTH", env)?;          // AUTH_ISSUER, AUTH_CLIENT_ID, AUTH_CLIENT_SECRET, …
let store = MySqlSessionStore::new(pool, SessionTables::with_prefix("crm_")?);
let bff = Bff::builder(config, store.clone(), |client, login| async move {
    // Runs after the login was verified: pick what the session keeps, or refuse.
    let access = client
        .verify_access_token(login.tokens.access_token.expose_secret(), client.client_id())
        .await
        .map_err(|_| LoginDenied::unavailable())?;
    if access.roles.is_empty() {
        return Err(LoginDenied::forbidden("Ihr Konto hat keinen Zugang."));
    }
    Ok(User { name: login.claims.name.clone().unwrap_or_default(), roles: access.roles })
})
.cookie_name("crm")                // __Host-crm_sid, __Host-crm_login
.base_path("/api/auth")            // the default: /api/auth/login, /callback, /logout, /backchannel-logout
.scopes(&["profile", "email", "roles"])
.client_ip(proxies.resolver())     // client address for the login limit (behind a proxy)
.build()?;

async fn me(session: Session<User>) -> axum::Json<Vec<String>> { axum::Json(session.data.roles) }

let app = Router::new()
    .route("/api/me", get(me))
    .with_state(bff.clone())                  // or `impl FromRef<AppState> for Bff<User>`
    .merge(bff.router())
    .layer(Csrf::header("x-requested-with", "crm")?.exempt_path("/api/auth/backchannel-logout"));
// In the worker: store.delete_expired(chrono::Utc::now()).await?;
```

- **Stored:** only the SHA-256 of the session and login cookie values. The login state (PKCE verifier, nonce)
  is sealed with AES-256-GCM under a key derived from the login cookie value, so a copy of the database
  does not reveal it and no server key is needed. No ID, access or refresh token is kept.
- **Cookies:** `__Host-<name>_sid` (`HttpOnly; Secure; Path=/`, `SameSite=Strict` or `Lax`) and
  `__Host-<name>_login` (10 minutes, `SameSite=Lax`, single use). Only a plain-HTTP loopback redirect URI
  (local development, see *Settings*) drops `Secure` and the prefix.
- **Rotation:** every login gets a new 256-bit session id; the session of the cookie the browser brought is
  deleted.
- **Lifetime:** idle and absolute timeout (`<PREFIX>_SESSION_IDLE_SECS`, `<PREFIX>_SESSION_MAX_SECS`); the
  last use is written at most once a minute. Expired sessions are deleted on access and by `delete_expired`.
- **`returnTo`:** only a path on the same origin (one leading `/`, no `//`, no `\`, visible ASCII, at most
  512 characters), otherwise `/`.
- **Logout:** `POST <base>/logout` (behind the app's `Csrf` layer; additionally refused when
  `Sec-Fetch-Site` is present and not `same-origin`) ends the session and answers `{"logoutUrl": "…"}` –
  the provider's logout with `post_logout_redirect_uri` (no `id_token_hint`, no token in a URL).
  `POST <base>/backchannel-logout` (exempt from CSRF) verifies the logout token and ends the sessions of its
  `sid` (if none has it: those of its `sub` without `sid`), without `sid` all of its `sub`; `200` or a bare
  `400`.
- **Errors:** login and callback answer with a short German HTML page (fixed texts, replaceable with
  `.error_page(…)`); the `Session` extractor with `401 {"code":"UNAUTHENTICATED",…}`. Use
  `Option<Session<T>>` or `bff.session(&headers)` for an own error shape.
- **Login limit:** each `GET <base>/login` writes a row before anyone is authenticated, so starts are limited
  per client address (IPv6 by /64) and in total, per minute and process (`<PREFIX>_LOGIN_RATE_PER_IP` = 30,
  `<PREFIX>_LOGIN_RATE_TOTAL` = 300, or `.login_limit(per_ip, total)`); more get `429` with the German page
  and write nothing, with `Retry-After: 60`. Pass `.client_ip(proxies.resolver())` behind a proxy (the
  default, the TCP peer, warns once when that peer is a loopback or private address); without a client
  address all share one counter (warned once). `.without_login_limit()` if the app limits it itself.
  Residual risk: a flood from many /64s can use up the total and block new logins for everyone for the rest
  of the minute (existing sessions are unaffected); counters are per instance (N instances → N× limit).
- **Settings:** `BffConfig` is built with `from_lookup`, `from_lookup_with` or `BffConfig::new`; `build()`
  checks the URLs again. URLs are `https://`; plain-HTTP loopback URLs (`http://localhost:…`) are refused
  unless the app allows them for development, and defaults it computes (client id, redirect URIs from its
  public URL) go into `Defaults`:

  ```rust,ignore
  let config = BffConfig::from_lookup_with("AUTH", env, Defaults {
      client_id: Some("crm".into()),
      redirect_uri: Some(format!("{public_url}/api/auth/callback")),
      post_logout_redirect_uri: Some(format!("{public_url}/")),
      allow_loopback_http: app_env == "development",   // never a constant `true`
  })?;
  // or: BffConfig::new(issuer, client_id, secret, redirect_uri)?.allow_loopback_http(app_env == "development")
  ```

Variables: [`docs/configuration.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/docs/configuration.md).

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/LICENSE-APACHE) or [MIT license](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/LICENSE-MIT) at your
option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this
crate by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional
terms or conditions.

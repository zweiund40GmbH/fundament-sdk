# z40-fundament-http

[![crates.io](https://img.shields.io/crates/v/z40-fundament-http.svg)](https://crates.io/crates/z40-fundament-http)
[![docs.rs](https://img.shields.io/docsrs/z40-fundament-http)](https://docs.rs/z40-fundament-http)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

HTTP groundwork for [axum](https://crates.io/crates/axum) 0.8 apps – the parts every app otherwise copies:
request ids, a trace layer that never logs secrets, the global log setup, security headers, `no-store`, the
client address behind trusted proxies and a CSRF header guard. Part of
[fundament-sdk](https://github.com/zweiund40GmbH/fundament-sdk); settings are read with the `from_lookup`
helpers of [`z40-fundament`](https://crates.io/crates/z40-fundament).

**Status:** pre-release (`0.x`). The API may still change before `1.0`.

## Features

| Feature | What it gives you |
|---|---|
| *(none)* | `SecurityHeaders` (CSP checked: no `'unsafe-inline'` for scripts, `frame-ancestors` required; `X-Frame-Options`, `nosniff`, `Referrer-Policy`, `Permissions-Policy`, `Cross-Origin-*`; no HSTS), `NoStore`, `TrustedProxies` / `ClientIp` (`X-Forwarded-For` only from trusted proxies), `Csrf` (custom header on state-changing requests, exempt paths) |
| `request-id` | `RequestIdLayer`: keeps a safe incoming `X-Request-Id` (≤ 64 of `[A-Za-z0-9._:-]`) or creates a UUID v7, echoes it, `RequestId` extension |
| `trace` | `trace::layer()`: span with method, route template and request id; status and latency at the end; never query strings, headers or bodies |
| `telemetry` | `TelemetryConfig`: JSON lines or one-line text, `EnvFilter`, from `<PREFIX>_LOG` (unset: `RUST_LOG`, then `info`) and `<PREFIX>_LOG_FORMAT` (`json` \| `text`); idempotent `init` |

## Quick start

```toml
[dependencies]
z40-fundament-http = { version = "0.4", features = ["request-id", "trace", "telemetry"] }
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

// The last `layer` call runs first: request id → trace → headers → no-store → CSRF → routes.
let app = routes
    .layer(Csrf::header("x-requested-with", "crm")?.protect_prefix("/api/").exempt_path("/api/webhook"))
    .layer(NoStore::new().prefix("/api/").prefix("/fundament/"))
    .layer(SecurityHeaders::api())
    .layer(trace::layer())
    .layer(RequestIdLayer);
axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;
```

- **Client address:** `ClientIp` is an extractor (`TrustedProxies` in the state via `FromRef`);
  `proxies.resolver()` is the `client_ip` function for `StatusEndpoint` and `FeedEndpoint` of
  `z40-fundament`, so their failure brake sees the same address.
- **CSRF answer:** `403 {"code":"CSRF","message":"Anfrage abgelehnt (CSRF-Schutz). Bitte laden Sie die Seite
  neu."}` – the error form and German fixed texts of the `z40-fundament` endpoints. Apps with their own error
  shape pass `.rejection(|| …)`; apps with another check (e.g. a double-submit cookie) use `Csrf::check(…)`.
- **CSP per route group:** headers are only added when missing. A route that sets its own CSP keeps it; a
  router group gets its own `SecurityHeaders` layer before it is merged.
- **HSTS** is the job of the edge proxy that terminates TLS.

Variables: [`docs/configuration.md`](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/docs/configuration.md).

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/LICENSE-APACHE) or [MIT license](https://github.com/zweiund40GmbH/fundament-sdk/blob/main/LICENSE-MIT) at your
option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this
crate by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional
terms or conditions.

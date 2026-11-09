//! The client address through axum: direct peer, trusted proxy chain, spoofing attempts, IPv6.
#![allow(clippy::unwrap_used)]

mod common;

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::extract::{ConnectInfo, FromRef};
use axum::http::Request;
use axum::http::request::Parts;
use axum::routing::get;
use std::net::{IpAddr, SocketAddr};
use tower::ServiceExt;
use z40_fundament_http::client_ip::{ClientIp, TrustedProxies};

#[derive(Clone)]
struct State {
    proxies: TrustedProxies,
}

impl FromRef<State> for TrustedProxies {
    fn from_ref(s: &State) -> Self {
        s.proxies.clone()
    }
}

/// The address the extractor sees for a request from `peer` with `xff` lines.
async fn seen(proxies: &str, peer: Option<&str>, xff: &[&str]) -> String {
    let state = State {
        proxies: TrustedProxies::parse(proxies).unwrap(),
    };
    let app = Router::new()
        .route(
            "/",
            get(|ClientIp(ip): ClientIp| async move { ip.to_string() }),
        )
        .with_state(state);
    let mut b = Request::builder().uri("/");
    for line in xff {
        b = b.header("x-forwarded-for", *line);
    }
    let mut req = b.body(Body::empty()).unwrap();
    if let Some(peer) = peer {
        let addr = SocketAddr::new(peer.parse().unwrap(), 40000);
        req.extensions_mut().insert(ConnectInfo(addr));
    }
    let res = app.oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

const PROXIES: &str = "10.0.0.0/8, fd00::/8";

#[tokio::test]
async fn direct_client() {
    assert_eq!(seen(PROXIES, Some("203.0.113.9"), &[]).await, "203.0.113.9");
    assert_eq!(
        seen("", Some("203.0.113.9"), &["1.2.3.4"]).await,
        "203.0.113.9",
        "no proxies"
    );
}

#[tokio::test]
async fn trusted_proxy_chain() {
    assert_eq!(
        seen(PROXIES, Some("10.0.0.2"), &["198.51.100.7"]).await,
        "198.51.100.7"
    );
    assert_eq!(
        seen(PROXIES, Some("10.0.0.2"), &["198.51.100.7, 10.0.0.5"]).await,
        "198.51.100.7",
        "second proxy hop skipped"
    );
}

#[tokio::test]
async fn spoofed_forwarded_for_is_ignored() {
    // An untrusted client sends its own header: ignored.
    assert_eq!(
        seen(PROXIES, Some("203.0.113.9"), &["10.0.0.1"]).await,
        "203.0.113.9"
    );
    // Through the proxy, a forged left part stays left of the real client.
    assert_eq!(
        seen(
            PROXIES,
            Some("10.0.0.2"),
            &["10.9.9.9, 6.6.6.6, 198.51.100.7"]
        )
        .await,
        "198.51.100.7"
    );
    // The proxy added a second header line instead of appending: the lines count as one list.
    assert_eq!(
        seen(PROXIES, Some("10.0.0.2"), &["6.6.6.6", "198.51.100.7"]).await,
        "198.51.100.7"
    );
    // An untrusted proxy in the chain ends the walk at it.
    assert_eq!(
        seen(PROXIES, Some("10.0.0.2"), &["6.6.6.6, 192.0.2.1, 10.0.0.3"]).await,
        "192.0.2.1"
    );
    // Garbage right of the client stops at the last trusted hop.
    assert_eq!(
        seen(PROXIES, Some("10.0.0.2"), &["6.6.6.6, unknown"]).await,
        "10.0.0.2"
    );
}

#[tokio::test]
async fn ipv6() {
    assert_eq!(
        seen(PROXIES, Some("fd00::1"), &["2001:db8::42"]).await,
        "2001:db8::42"
    );
    assert_eq!(
        seen(PROXIES, Some("2001:db8::1"), &["2001:db8::42"]).await,
        "2001:db8::1"
    );
    assert_eq!(
        seen(PROXIES, Some("::ffff:10.0.0.2"), &["198.51.100.7"]).await,
        "198.51.100.7"
    );
}

#[tokio::test]
async fn without_connect_info() {
    let (logs, _guard) = common::capture_logs();
    assert_eq!(seen(PROXIES, None, &["198.51.100.7"]).await, "0.0.0.0");
    assert_eq!(seen(PROXIES, None, &[]).await, "0.0.0.0");
    let text = logs.text();
    assert_eq!(
        text.matches("cannot see client addresses").count(),
        1,
        "warns once: {text}"
    );
    assert!(text.contains("WARN"), "{text}");
    let parts: Parts = Request::builder().body(()).unwrap().into_parts().0;
    let resolve = TrustedProxies::parse(PROXIES).unwrap().resolver();
    assert_eq!(
        resolve(&parts),
        None::<IpAddr>,
        "the endpoints of z40-fundament then warn"
    );
}

/// The extractor and the resolver of a router with axum's `MockConnectInfo` layer, as in app tests.
async fn seen_mocked(peer: &str, xff: Option<&str>, real: Option<&str>) -> (String, String) {
    let proxies = TrustedProxies::parse(PROXIES).unwrap();
    let resolve = std::sync::Arc::new(proxies.resolver());
    let peer = SocketAddr::new(peer.parse().unwrap(), 40000);
    let app = Router::new()
        .route(
            "/",
            get(
                move |ClientIp(ip): ClientIp, req: Request<Body>| async move {
                    let resolved = resolve(&req.into_parts().0).map(|ip| ip.to_string());
                    format!("{ip} {}", resolved.unwrap_or_default())
                },
            ),
        )
        .with_state(State { proxies })
        .layer(MockConnectInfo(peer));
    let mut b = Request::builder().uri("/");
    if let Some(xff) = xff {
        b = b.header("x-forwarded-for", xff);
    }
    let mut req = b.body(Body::empty()).unwrap();
    if let Some(real) = real {
        let addr = SocketAddr::new(real.parse().unwrap(), 40000);
        req.extensions_mut().insert(ConnectInfo(addr));
    }
    let res = app.oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let (a, b) = text.split_once(' ').unwrap();
    (a.to_owned(), b.to_owned())
}

#[tokio::test]
async fn mock_connect_info_in_router_tests() {
    let both = |s: &str| (s.to_owned(), s.to_owned());
    assert_eq!(
        seen_mocked("203.0.113.9", None, None).await,
        both("203.0.113.9")
    );
    assert_eq!(
        seen_mocked("203.0.113.9", Some("6.6.6.6"), None).await,
        both("203.0.113.9"),
        "untrusted mocked peer: header ignored"
    );
    assert_eq!(
        seen_mocked("10.0.0.2", Some("198.51.100.7"), None).await,
        both("198.51.100.7"),
        "mocked proxy peer: header walked"
    );
    assert_eq!(
        seen_mocked("10.0.0.2", Some("198.51.100.7"), Some("203.0.113.9")).await,
        both("203.0.113.9"),
        "a real ConnectInfo wins"
    );
}

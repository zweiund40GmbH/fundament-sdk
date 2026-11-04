//! Both sides of the status endpoint: the library's `StatusEndpoint` (axum on 127.0.0.1) read by
//! `StatusClient`, plus stub servers for slow, oversized, redirecting and broken answers. No database.
#![cfg(all(feature = "status", feature = "status-client"))]
// Test helpers fail fast with unwrap, like the test functions themselves.
#![allow(clippy::unwrap_used)]

use axum::Router;
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;
use chrono::Utc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use z40_fundament::credentials::{BearerKey, KeyList, MAX_FAILURES, key_entry};
use z40_fundament::protocol::{Level, STATUS_PATH};
use z40_fundament::status::{
    FETCH_BODY_MAX, StatusBuilder, StatusClient, StatusDocument, StatusEndpoint, StatusFetchError,
    StatusProvider,
};

const KEY: &str = "status-key-of-the-monitor-0123456789";

/// A provider with one note naming the caller.
struct Provider;

impl StatusProvider for Provider {
    async fn status(
        &self,
        caller: &str,
    ) -> Result<StatusDocument, z40_fundament::status::handler::BoxError> {
        Ok(StatusBuilder::new("billing", "1.4.0", Utc::now())
            .note(Level::Warning, format!("Abruf durch {caller}"))
            .build())
    }
}

/// Serves `router` on 127.0.0.1 (with the peer address for the brake); returns the base URL.
async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let service = router.into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    format!("http://{addr}")
}

fn client(base: &str, key: &str) -> StatusClient {
    StatusClient::plain_http_for_tests(base, BearerKey::new(key).unwrap()).unwrap()
}

/// A raw HTTP/1.1 server answering every connection with a chunked body: `chunks` chunks of `size`
/// bytes, waiting `pause` before each (no `Content-Length`, so the client must count).
async fn chunked_server(chunks: usize, size: usize, pause: Duration) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            tokio::spawn(async move {
                // Read the request head first: closing a socket with unread data resets it.
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.ends_with(b"\r\n\r\n") {
                    let n = conn.read(&mut buf).await?;
                    if n == 0 {
                        return Ok(());
                    }
                    head.extend_from_slice(&buf[..n]);
                }
                let answer = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                              transfer-encoding: chunked\r\n\r\n";
                conn.write_all(answer.as_bytes()).await?;
                for _ in 0..chunks {
                    tokio::time::sleep(pause).await;
                    let chunk = format!("{size:x}\r\n{}\r\n", " ".repeat(size));
                    conn.write_all(chunk.as_bytes()).await?;
                }
                conn.write_all(b"0\r\n\r\n").await?;
                // Keep the connection until the client is done with it.
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok::<_, std::io::Error>(())
            });
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn round_trip_wrong_key_and_brake() {
    let keys = KeyList::parse(&key_entry("monitor", KEY).unwrap()).unwrap();
    let base = serve(StatusEndpoint::new(Provider, keys).router()).await;
    let doc = client(&base, KEY).fetch().await.unwrap();
    assert_eq!(
        (doc.app.as_str(), doc.version.as_str()),
        ("billing", "1.4.0")
    );
    assert_eq!(doc.state, Level::Warning);
    assert_eq!(doc.notes[0].text, "Abruf durch monitor");
    let wrong = client(&base, "wrong-key");
    for _ in 0..MAX_FAILURES {
        assert_eq!(wrong.fetch().await, Err(StatusFetchError::Status(401)));
    }
    assert_eq!(
        client(&base, KEY).fetch().await,
        Err(StatusFetchError::Status(429)),
        "braked even with the right key"
    );
    assert_eq!(StatusFetchError::Status(429).text(), "HTTP 429");
}

#[tokio::test]
async fn slow_answer_is_a_timeout_within_the_deadline() {
    let slow = Router::new().route(
        STATUS_PATH,
        get(|| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            "{}"
        }),
    );
    for base in [
        serve(slow).await,
        chunked_server(10, 10, Duration::from_millis(400)).await,
    ] {
        let started = Instant::now();
        assert_eq!(
            client(&base, KEY).fetch().await,
            Err(StatusFetchError::Timeout)
        );
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(1900) && took < Duration::from_millis(3000),
            "{took:?}"
        );
    }
}

/// Serves the fixed `answer` at the status path; returns the base URL.
async fn stub(answer: axum::response::Response) -> String {
    let body = axum::body::to_bytes(answer.into_body(), usize::MAX)
        .await
        .unwrap();
    serve(Router::new().route(STATUS_PATH, get(move || async move { body.clone() }))).await
}

#[tokio::test]
async fn oversized_redirecting_and_broken_answers() {
    let fetch = |base: String| async move { client(&base, KEY).fetch().await };
    let big = "x".repeat(FETCH_BODY_MAX + 1).into_response();
    assert_eq!(fetch(stub(big).await).await, Err(StatusFetchError::Invalid));
    let html = "<html>".into_response();
    assert_eq!(
        fetch(stub(html).await).await,
        Err(StatusFetchError::Invalid)
    );
    let bad = r#"{"app":"billing","zustand":"rot"}"#.into_response();
    assert_eq!(fetch(stub(bad).await).await, Err(StatusFetchError::Invalid));
    let lenient = r#"{"app":"billing","zustand":"ok","neu":1}"#.into_response();
    assert_eq!(fetch(stub(lenient).await).await.unwrap().app, "billing");
    let moved = Router::new().route(
        STATUS_PATH,
        get(|| async { Redirect::temporary("/elsewhere") }),
    );
    assert_eq!(
        fetch(serve(moved).await).await,
        Err(StatusFetchError::Status(307)),
        "redirects are not followed"
    );
    let chunked = chunked_server(FETCH_BODY_MAX / 4096 + 2, 4096, Duration::ZERO).await;
    assert_eq!(
        fetch(chunked).await,
        Err(StatusFetchError::Invalid),
        "a body without length is counted"
    );
    assert_eq!(
        fetch("http://127.0.0.1:9".to_owned()).await,
        Err(StatusFetchError::Unreachable)
    );
}

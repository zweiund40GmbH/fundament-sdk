//! Transport-level behaviour of the GraphQL client: redirects, timeout, size limit, connection errors.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn redirects_are_not_followed() {
    let router = Router::new()
        .route(
            "/fundament/v1/graphql",
            post(|| async { Redirect::temporary("/other") }),
        )
        .route("/other", post(|| async { StatusCode::IM_A_TEAPOT }));
    let origin = serve(router).await;
    let err = client(&origin, GraphqlClientOptions::default())
        .query::<_, Data>(QUERY, &vars())
        .await
        .unwrap_err();
    assert_eq!(
        err,
        QueryError::Status {
            status: 307,
            errors: vec![]
        }
    );
}

#[tokio::test]
async fn slow_answer_times_out() {
    let router = Router::new().route(
        "/fundament/v1/graphql",
        post(|| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            "{}"
        }),
    );
    let origin = serve(router).await;
    let options = GraphqlClientOptions {
        timeout: Duration::from_millis(200),
        ..GraphqlClientOptions::default()
    };
    let err = client(&origin, options)
        .query::<_, Data>(QUERY, &vars())
        .await
        .unwrap_err();
    assert_eq!(err, QueryError::Timeout);
}

#[tokio::test]
async fn stalled_body_times_out() {
    // Headers arrive, the promised body never does: the overall timeout still applies.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await;
        let head = "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{\"data\":";
        let _ = socket.write_all(head.as_bytes()).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let options = GraphqlClientOptions {
        timeout: Duration::from_millis(300),
        ..GraphqlClientOptions::default()
    };
    let err = client(&origin, options)
        .query::<_, Data>(QUERY, &vars())
        .await
        .unwrap_err();
    assert_eq!(err, QueryError::Timeout);
}

#[tokio::test]
async fn oversized_answer_is_refused() {
    let (origin, _) = answering(
        StatusCode::OK,
        r#"{"data":{"site":{"name":"a very long name indeed"}}}"#,
    )
    .await;
    let options = GraphqlClientOptions {
        max_response_bytes: 20,
        ..GraphqlClientOptions::default()
    };
    let err = client(&origin, options)
        .query::<_, Data>(QUERY, &vars())
        .await
        .unwrap_err();
    assert_eq!(err, QueryError::TooLarge);
}

#[tokio::test]
async fn streamed_oversized_answer_is_refused() {
    // Chunked, so there is no `Content-Length`: the limit applies to what is actually sent.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await;
        let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
        let _ = socket.write_all(head.as_bytes()).await;
        for _ in 0..50 {
            let chunk = format!("64\r\n{}\r\n", "x".repeat(100));
            if socket.write_all(chunk.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let options = GraphqlClientOptions {
        max_response_bytes: 1000,
        ..GraphqlClientOptions::default()
    };
    let err = client(&origin, options)
        .query::<_, Data>(QUERY, &vars())
        .await
        .unwrap_err();
    assert_eq!(err, QueryError::TooLarge);
}

#[tokio::test]
async fn unreachable_server_and_secret_hygiene() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", l.local_addr().unwrap());
    drop(l);
    let c = client(&origin, GraphqlClientOptions::default());
    let err = c.query::<_, Data>(QUERY, &vars()).await.unwrap_err();
    assert_eq!(err, QueryError::Unreachable);
    for text in [format!("{c:?}"), format!("{err:?}"), err.to_string()] {
        assert!(!text.contains(KEY), "{text}");
    }
}

#[test]
fn production_clients_need_https() {
    let key = || BearerKey::new(KEY).unwrap();
    assert_eq!(
        GraphqlClient::new("http://127.0.0.1:1", key()).unwrap_err(),
        ClientBuildError::InvalidUrl
    );
}

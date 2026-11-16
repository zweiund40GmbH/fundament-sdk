//! The GraphQL client against local axum servers: success, variables, key header, GraphQL errors,
//! partial data and HTTP statuses (here); redirects, timeouts, size limit and connection errors
//! (`transport.rs`). No database.
#![cfg(feature = "graphql-client")]
// Test helpers fail fast with unwrap, like the test functions themselves.
#![allow(clippy::unwrap_used)]

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Redirect;
use axum::routing::post;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use z40_fundament::credentials::BearerKey;
use z40_fundament::graphql_client::{
    ClientBuildError, GraphqlClient, GraphqlClientOptions, MAX_ERRORS, QueryError,
};

mod transport;

const KEY: &str = "router-key-0123456789";
const QUERY: &str = "query Site($id: ID!) { site(id: $id) { name } }";

#[derive(Serialize)]
struct Vars {
    id: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Data {
    site: Option<Site>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Site {
    name: String,
}

/// What the server saw of the last request.
#[derive(Default)]
struct Seen {
    authorization: Option<String>,
    content_type: Option<String>,
    body: Option<Value>,
    query_string: Option<String>,
}

type Shared = Arc<Mutex<Seen>>;

/// Serves `router` on 127.0.0.1; returns its origin.
async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

/// A server that records the request and answers with `status` and `body`.
async fn answering(status: StatusCode, body: &'static str) -> (String, Shared) {
    let seen = Shared::default();
    let router = Router::new()
        .route(
            "/fundament/v1/graphql",
            post(
                move |State(seen): State<Shared>,
                      uri: axum::http::Uri,
                      headers: HeaderMap,
                      payload: axum::body::Bytes| async move {
                    let header = |n| headers.get(n).map(|v| v.to_str().unwrap().to_owned());
                    *seen.lock().unwrap() = Seen {
                        authorization: header(header::AUTHORIZATION),
                        content_type: header(header::CONTENT_TYPE),
                        body: serde_json::from_slice(&payload).ok(),
                        query_string: uri.query().map(str::to_owned),
                    };
                    (status, [(header::CONTENT_TYPE, "application/json")], body)
                },
            ),
        )
        .with_state(Arc::clone(&seen));
    (serve(router).await, seen)
}

fn client(origin: &str, options: GraphqlClientOptions) -> GraphqlClient {
    GraphqlClient::plain_http_for_tests(origin, BearerKey::new(KEY).unwrap(), options).unwrap()
}

fn vars() -> Vars {
    Vars { id: "42".into() }
}

#[tokio::test]
async fn query_sends_key_and_variables_and_types_the_data() {
    let (origin, seen) = answering(StatusCode::OK, r#"{"data":{"site":{"name":"Main"}}}"#).await;
    let c = client(&origin, GraphqlClientOptions::default());
    let data: Data = c.query(QUERY, &vars()).await.unwrap();
    assert_eq!(
        data.site,
        Some(Site {
            name: "Main".into()
        })
    );
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.authorization.as_deref(),
        Some(&*format!("Bearer {KEY}"))
    );
    assert_eq!(seen.content_type.as_deref(), Some("application/json"));
    assert_eq!(
        seen.query_string, None,
        "the key and query are never in the URL"
    );
    assert_eq!(
        seen.body,
        Some(json!({"query": QUERY, "variables": {"id": "42"}}))
    );
}

#[tokio::test]
async fn no_variables_are_left_out() {
    let (origin, seen) = answering(StatusCode::OK, r#"{"data":{"site":null}}"#).await;
    let c = client(&origin, GraphqlClientOptions::default());
    let data: Data = c.query("{ site { name } }", &()).await.unwrap();
    assert_eq!(data.site, None);
    assert_eq!(
        seen.lock().unwrap().body,
        Some(json!({"query": "{ site { name } }"}))
    );
}

#[tokio::test]
async fn custom_path() {
    let router = Router::new().route("/graphql", post(|| async { r#"{"data":{"site":null}}"# }));
    let origin = serve(router).await;
    let options = GraphqlClientOptions {
        path: "/graphql".into(),
        ..GraphqlClientOptions::default()
    };
    let data: Data = client(&origin, options)
        .query(QUERY, &vars())
        .await
        .unwrap();
    assert_eq!(data.site, None);
}

#[tokio::test]
async fn graphql_errors_with_codes_are_an_error_in_strict_mode() {
    let body = r#"{"data":{"site":null},"errors":[{"message":"not allowed","extensions":{"code":"FORBIDDEN"}}]}"#;
    let (origin, _) = answering(StatusCode::OK, body).await;
    let err = client(&origin, GraphqlClientOptions::default())
        .query::<_, Data>(QUERY, &vars())
        .await
        .unwrap_err();
    assert!(err.has_code("FORBIDDEN"));
    assert_eq!(err.errors()[0].message, "not allowed");
    assert!(matches!(err, QueryError::Graphql { .. }));
}

#[tokio::test]
async fn execute_keeps_partial_data() {
    let body = r#"{"data":{"site":{"name":"Main"}},"errors":[{"message":"other field failed"}]}"#;
    let (origin, _) = answering(StatusCode::OK, body).await;
    let r = client(&origin, GraphqlClientOptions::default())
        .execute::<_, Data>(QUERY, &vars())
        .await
        .unwrap();
    assert_eq!(
        r.data,
        Some(Data {
            site: Some(Site {
                name: "Main".into()
            })
        })
    );
    assert_eq!(r.errors.len(), 1);
    assert_eq!(r.errors[0].code, None);
}

#[tokio::test]
async fn execute_without_data_needs_errors() {
    let c = |body| async move {
        let (origin, _) = answering(StatusCode::OK, body).await;
        client(&origin, GraphqlClientOptions::default())
            .execute::<_, Data>(QUERY, &vars())
            .await
    };
    let r = c(r#"{"data":null,"errors":[{"message":"x","extensions":{"code":"INTERNAL"}}]}"#)
        .await
        .unwrap();
    assert!(r.data.is_none() && r.errors.len() == 1);
    assert_eq!(c(r#"{}"#).await.unwrap_err(), QueryError::Decode);
}

#[tokio::test]
async fn errors_are_bounded() {
    let body: &'static str = Box::leak(
        format!(
            r#"{{"errors":[{}]}}"#,
            vec![r#"{"message":"x"}"#; MAX_ERRORS + 10].join(",")
        )
        .into_boxed_str(),
    );
    let (origin, _) = answering(StatusCode::OK, body).await;
    let err = client(&origin, GraphqlClientOptions::default())
        .query::<_, Data>(QUERY, &vars())
        .await
        .unwrap_err();
    assert_eq!(err.errors().len(), MAX_ERRORS);
}

#[tokio::test]
async fn http_statuses() {
    let (origin, _) = answering(StatusCode::UNAUTHORIZED, "nope").await;
    let err = client(&origin, GraphqlClientOptions::default())
        .query::<_, Data>(QUERY, &vars())
        .await
        .unwrap_err();
    assert_eq!(
        err,
        QueryError::Status {
            status: 401,
            errors: vec![]
        }
    );

    let body = r#"{"errors":[{"message":"too deep","extensions":{"code":"VALIDATION"}}]}"#;
    let (origin, _) = answering(StatusCode::BAD_REQUEST, body).await;
    let err = client(&origin, GraphqlClientOptions::default())
        .query::<_, Data>(QUERY, &vars())
        .await
        .unwrap_err();
    assert!(matches!(err, QueryError::Status { status: 400, .. }));
    assert!(err.has_code("VALIDATION"));
}

#[tokio::test]
async fn undecodable_answers() {
    for body in [
        "not json",
        r#"{"data":{"site":{"name":5}}}"#,
        r#"{"data":[1]}"#,
    ] {
        let (origin, _) = answering(StatusCode::OK, body).await;
        let err = client(&origin, GraphqlClientOptions::default())
            .query::<_, Data>(QUERY, &vars())
            .await
            .unwrap_err();
        assert_eq!(err, QueryError::Decode, "{body}");
    }
}

#[tokio::test]
async fn answers_without_data_and_errors_are_undecodable() {
    for body in [r#"{}"#, r#"{"errors":[]}"#, r#"{"data":null}"#] {
        let (origin, _) = answering(StatusCode::OK, body).await;
        let c = client(&origin, GraphqlClientOptions::default());
        let e = c.query::<_, Option<Data>>(QUERY, &vars()).await;
        assert_eq!(e.unwrap_err(), QueryError::Decode, "{body}");
        let e = c.query::<_, ()>(QUERY, &vars()).await;
        assert_eq!(e.unwrap_err(), QueryError::Decode, "{body}");
    }
}

#[tokio::test]
async fn variables_must_be_an_object_or_null() {
    let (origin, seen) = answering(StatusCode::OK, r#"{"data":{"site":null}}"#).await;
    let c = client(&origin, GraphqlClientOptions::default());
    for bad in [json!([1]), json!("x"), json!(5)] {
        let e = c.query::<_, Data>(QUERY, &bad).await;
        assert_eq!(e.unwrap_err(), QueryError::Encode, "{bad}");
    }
    assert!(seen.lock().unwrap().body.is_none(), "nothing was sent");
}

#[tokio::test]
async fn accept_header_prefers_graphql_response_json() {
    let router = Router::new().route(
        "/fundament/v1/graphql",
        post(|headers: HeaderMap| async move {
            let accept = headers.get(header::ACCEPT).unwrap().to_str().unwrap();
            assert_eq!(
                accept,
                "application/graphql-response+json, application/json"
            );
            r#"{"data":{"site":null}}"#
        }),
    );
    let origin = serve(router).await;
    let r = client(&origin, GraphqlClientOptions::default())
        .query::<_, Data>(QUERY, &vars())
        .await;
    assert!(r.is_ok());
}

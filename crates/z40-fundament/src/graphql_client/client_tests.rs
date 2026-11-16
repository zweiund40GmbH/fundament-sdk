//! Unit tests of the client: origins, paths, redaction, request body.

use super::*;
use crate::graphql_client::ResponseError;

fn key() -> BearerKey {
    BearerKey::new("s3cr3t").unwrap()
}

#[test]
fn origins_paths_and_redaction() {
    let c = GraphqlClient::new("https://router.example.com/", key()).unwrap();
    let d = format!("{c:?}");
    assert!(d.contains("https://router.example.com/fundament/v1/graphql"));
    assert!(!d.contains("s3cr3t"));
    for bad in [
        "http://router.example.com",
        "https://router.example.com/x",
        "x",
    ] {
        assert_eq!(
            GraphqlClient::new(bad, key()).unwrap_err(),
            ClientBuildError::InvalidUrl
        );
    }
    for bad in [
        "graphql",
        "//evil.example.com",
        "/a?b=1",
        "/a#b",
        "/a b",
        "",
    ] {
        let options = GraphqlClientOptions {
            path: bad.to_owned(),
            ..GraphqlClientOptions::default()
        };
        assert_eq!(
            GraphqlClient::with_options("https://router.example.com", key(), options).unwrap_err(),
            ClientBuildError::InvalidPath,
            "{bad:?}"
        );
    }
    let options = GraphqlClientOptions {
        path: "/graphql".to_owned(),
        ..GraphqlClientOptions::default()
    };
    assert!(GraphqlClient::with_options("https://router.example.com", key(), options).is_ok());
}

#[test]
fn test_origin_is_loopback_only() {
    let o = GraphqlClientOptions::default;
    assert!(GraphqlClient::plain_http_for_tests("http://127.0.0.1:1", key(), o()).is_ok());
    assert!(GraphqlClient::plain_http_for_tests("http://router.example.com", key(), o()).is_err());
}

#[test]
fn null_variables_are_left_out() {
    let none = serde_json::to_value(()).unwrap();
    let body = serde_json::to_string(&RequestBody {
        query: "{ a }",
        variables: &none,
    })
    .unwrap();
    assert_eq!(body, r#"{"query":"{ a }"}"#);
}

#[test]
fn errors_expose_codes() {
    let e = QueryError::Status {
        status: 400,
        errors: vec![ResponseError {
            message: "m".into(),
            code: Some("VALIDATION".into()),
        }],
    };
    assert!(e.has_code("VALIDATION") && !e.has_code("TIMEOUT"));
    assert_eq!(e.to_string(), "GraphQL server answered with HTTP 400");
    assert!(QueryError::Timeout.errors().is_empty());
}

#[test]
fn only_objects_and_null_are_variables() {
    let ok = |v: Value| matches!(v, Value::Null | Value::Object(_));
    assert!(ok(serde_json::to_value(()).unwrap()));
    assert!(ok(serde_json::json!({"a": 1})));
    for bad in [
        serde_json::json!([1]),
        serde_json::json!("s"),
        serde_json::json!(5),
    ] {
        assert!(!ok(bad));
    }
}

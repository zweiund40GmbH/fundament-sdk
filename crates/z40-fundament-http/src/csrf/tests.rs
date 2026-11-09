//! Tests of [`super`].

use super::*;

fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(*k, HeaderValue::from_static(v));
    }
    h
}

#[test]
fn which_requests_are_checked() {
    let c = Csrf::header("x-requested-with", "crm")
        .unwrap()
        .protect_prefix("/api/")
        .protect_path("/auth/logout")
        .exempt_path("/api/auth/backchannel-logout");
    assert!(c.requires_check(&Method::POST, "/api/graphql"));
    assert!(
        c.requires_check(&Method::DELETE, "/api/x") && c.requires_check(&Method::PATCH, "/api/x")
    );
    assert!(c.requires_check(&Method::POST, "/auth/logout"));
    assert!(!c.requires_check(&Method::POST, "/api/auth/backchannel-logout"));
    assert!(
        !c.requires_check(&Method::GET, "/api/me") && !c.requires_check(&Method::OPTIONS, "/api/x")
    );
    assert!(!c.requires_check(&Method::POST, "/healthz"));
}

#[test]
fn header_must_match_exactly() {
    let c = Csrf::header("X-Requested-With", "crm").unwrap();
    let post = Method::POST;
    assert!(!c.allows(&post, "/x", &HeaderMap::new()));
    assert!(!c.allows(
        &post,
        "/x",
        &headers(&[("x-requested-with", "XMLHttpRequest")])
    ));
    assert!(!c.allows(&post, "/x", &headers(&[("x-requested-with", "CRM")])));
    assert!(c.allows(&post, "/x", &headers(&[("x-requested-with", "crm")])));
    assert!(c.allows(&Method::GET, "/x", &HeaderMap::new()));
}

#[test]
fn static_header_matches_like_the_checked_one() {
    const H: CsrfHeader = CsrfHeader::new("x-requested-with", "crm");
    let c = Csrf::header_static(H);
    let post = Method::POST;
    assert!(c.allows(&post, "/x", &headers(&[("x-requested-with", "crm")])));
    assert!(!c.allows(&post, "/x", &headers(&[("x-requested-with", "CRM")])));
    assert!(!c.allows(&post, "/x", &HeaderMap::new()));
    assert_eq!((H.name(), H.value()), ("x-requested-with", "crm"));
}

#[test]
fn invalid_configuration() {
    for safelisted in [
        "Content-Type",
        "accept",
        "accept-language",
        "content-language",
        "range",
    ] {
        assert_eq!(
            Csrf::header(safelisted, "crm").unwrap_err(),
            CsrfConfigError::Name,
            "{safelisted}"
        );
    }
    let c = Csrf::header("x-csrf", "1").unwrap().protect_prefix("/auth");
    assert!(c.requires_check(&Method::POST, "/auth/logout"));
    assert!(!c.requires_check(&Method::POST, "/authority"));
    assert_eq!(
        Csrf::header("bad name", "x").unwrap_err(),
        CsrfConfigError::Name
    );
    assert_eq!(
        Csrf::header("x-csrf", "").unwrap_err(),
        CsrfConfigError::Value
    );
    assert_eq!(
        Csrf::header("x-csrf", "a b").unwrap_err(),
        CsrfConfigError::Value
    );
}

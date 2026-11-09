//! Tests of `BffConfig`.

use super::*;
use z40_fundament::config::ConfigErrorKind;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
}

const BASE: [(&str, &str); 4] = [
    ("AUTH_ISSUER", "https://login.example.com/"),
    ("AUTH_CLIENT_ID", "crm"),
    ("AUTH_CLIENT_SECRET", "s3cr3t"),
    ("AUTH_REDIRECT_URI", "https://crm.example.com/auth/callback"),
];

fn with(extra: &[(&str, &str)]) -> Result<BffConfig, ConfigError> {
    let mut all: Vec<(&str, &str)> = BASE.to_vec();
    all.retain(|(k, _)| !extra.iter().any(|(e, _)| e == k));
    all.extend_from_slice(extra);
    BffConfig::from_lookup("AUTH", env(&all))
}

#[test]
fn defaults_and_redacted_secret() {
    let c = with(&[]).unwrap();
    assert_eq!(c.issuer, "https://login.example.com");
    assert_eq!(c.client_secret.expose(), "s3cr3t");
    assert_eq!(c.same_site, SameSite::Strict);
    assert_eq!(c.timeouts, Timeouts::from_secs(7200, 36_000));
    assert_eq!(c.post_logout_redirect_uri, None);
    assert!(c.secure_cookies());
    assert!(!format!("{c:?}").contains("s3cr3t"));
}

#[test]
fn optional_settings() {
    let c = with(&[
        ("AUTH_POST_LOGOUT_REDIRECT_URI", "https://crm.example.com/"),
        ("AUTH_COOKIE_SAMESITE", "lax"),
        ("AUTH_SESSION_IDLE_SECS", "900"),
        ("AUTH_SESSION_MAX_SECS", "3600"),
    ])
    .unwrap();
    assert_eq!(
        c.post_logout_redirect_uri.as_deref(),
        Some("https://crm.example.com/")
    );
    assert_eq!(c.same_site, SameSite::Lax);
    assert_eq!(c.timeouts, Timeouts::from_secs(900, 3600));
}

#[test]
fn required_values_and_bad_values_are_refused() {
    for missing in [
        "AUTH_ISSUER",
        "AUTH_CLIENT_ID",
        "AUTH_CLIENT_SECRET",
        "AUTH_REDIRECT_URI",
    ] {
        let err = with(&[(missing, " ")]).unwrap_err();
        assert_eq!(err.name, missing);
    }
    let err = with(&[("AUTH_COOKIE_SAMESITE", "none")]).unwrap_err();
    assert_eq!(err.name, "AUTH_COOKIE_SAMESITE");
    let err = with(&[("AUTH_SESSION_IDLE_SECS", "5")]).unwrap_err();
    assert!(matches!(err.kind, ConfigErrorKind::OutOfRange { .. }));
    let err = with(&[("AUTH_REDIRECT_URI", "http://crm.example.com/cb?k=s3cr3t")]).unwrap_err();
    assert_eq!(err.kind, ConfigErrorKind::InvalidUrl);
    assert!(!err.to_string().contains("s3cr3t"), "value never echoed");
}

#[test]
fn plain_http_only_on_loopback_when_allowed_and_then_without_secure_cookies() {
    let local = [
        ("AUTH_ISSUER", "http://127.0.0.1:8400"),
        ("AUTH_REDIRECT_URI", "http://localhost:3000/auth/callback"),
    ];
    let err = with(&local).unwrap_err();
    assert_eq!(
        (err.name.as_str(), &err.kind),
        ("AUTH_ISSUER", &ConfigErrorKind::InvalidUrl)
    );
    let err = with(&local[1..]).unwrap_err();
    assert_eq!(err.name, "AUTH_REDIRECT_URI", "refused by default");
    let mut all: Vec<(&str, &str)> = BASE.to_vec();
    all.retain(|(k, _)| !local.iter().any(|(e, _)| e == k));
    all.extend_from_slice(&local);
    let dev = Defaults {
        allow_loopback_http: true,
        ..Defaults::default()
    };
    let c = BffConfig::from_lookup_with("AUTH", env(&all), dev).unwrap();
    assert!(c.allow_loopback_http && !c.secure_cookies());
    assert_eq!(c.check(), Ok(()));
    let c = c.allow_loopback_http(false);
    assert_eq!(
        c.check(),
        Err(InvalidUrl("issuer")),
        "checked again at build"
    );
    for ok in [
        "https://a.example",
        "http://localhost:1/x",
        "http://[::1]/",
        "http://127.0.0.2/",
    ] {
        assert_eq!(check_url(ok, true), Ok(()), "{ok}");
        assert_eq!(
            check_url(ok, false).is_ok(),
            ok.starts_with("https:"),
            "{ok}"
        );
    }
    for bad in [
        "http://a.example/",
        "http://localhost.example.com/",
        "http://127.example/",
        "https://user:pw@a.example/",
        "http://user@localhost/",
        "https://a.example/#x",
        "ftp://localhost/",
        "javascript:alert(1)",
        "/relative",
        "https://",
    ] {
        assert!(check_url(bad, true).is_err(), "{bad}");
    }
}

#[test]
fn app_defaults_fill_unset_variables() {
    let defaults = || Defaults {
        client_id: Some("crm".into()),
        redirect_uri: Some("https://crm.example.com/api/auth/callback".into()),
        post_logout_redirect_uri: Some("https://crm.example.com/".into()),
        allow_loopback_http: false,
    };
    let only = env(&[
        ("AUTH_ISSUER", "https://login.example.com"),
        ("AUTH_CLIENT_SECRET", "s3cr3t"),
    ]);
    let c = BffConfig::from_lookup_with("AUTH", only, defaults()).unwrap();
    assert_eq!(c.client_id, "crm");
    assert_eq!(c.redirect_uri, "https://crm.example.com/api/auth/callback");
    assert_eq!(
        c.post_logout_redirect_uri.as_deref(),
        Some("https://crm.example.com/")
    );
    assert!(!c.allow_loopback_http);
    let c = BffConfig::from_lookup_with(
        "AUTH",
        env(&[
            ("AUTH_ISSUER", "https://login.example.com"),
            ("AUTH_CLIENT_SECRET", "s3cr3t"),
            ("AUTH_CLIENT_ID", "billing"),
            ("AUTH_REDIRECT_URI", " "),
        ]),
        defaults(),
    )
    .unwrap();
    assert_eq!(c.client_id, "billing", "the variable wins");
    assert_eq!(
        c.redirect_uri, "https://crm.example.com/api/auth/callback",
        "empty = unset"
    );
    let bad = Defaults {
        redirect_uri: Some("http://crm.example.com/cb?k=s3cr3t".into()),
        ..defaults()
    };
    let err = BffConfig::from_lookup_with("AUTH", env(&BASE[..3]), bad).unwrap_err();
    assert_eq!(
        (err.name.as_str(), &err.kind),
        ("AUTH_REDIRECT_URI", &ConfigErrorKind::InvalidUrl)
    );
    assert!(!err.to_string().contains("s3cr3t"), "default never echoed");
    let err =
        BffConfig::from_lookup_with("AUTH", env(&BASE[2..]), Defaults::default()).unwrap_err();
    assert_eq!(err.name, "AUTH_ISSUER", "the issuer has no default");
}

#[test]
fn login_rates_and_constructor() {
    let c = with(&[]).unwrap();
    assert_eq!((c.login_rate_per_ip, c.login_rate_total), (30, 300));
    let c = with(&[
        ("AUTH_LOGIN_RATE_PER_IP", "5"),
        ("AUTH_LOGIN_RATE_TOTAL", "50"),
    ])
    .unwrap();
    assert_eq!((c.login_rate_per_ip, c.login_rate_total), (5, 50));
    let err = with(&[("AUTH_LOGIN_RATE_PER_IP", "0")]).unwrap_err();
    assert!(matches!(err.kind, ConfigErrorKind::OutOfRange { .. }));

    let secret = || SecretString::new("s3cr3t");
    let n = BffConfig::new(
        "https://login.example.com/",
        "crm",
        secret(),
        "https://crm.example.com/cb",
    )
    .unwrap();
    assert_eq!(n.issuer, "https://login.example.com");
    assert_eq!((n.login_rate_per_ip, n.same_site), (30, SameSite::Strict));
    assert_eq!(
        BffConfig::new(
            "https://l.example",
            "crm",
            secret(),
            "http://crm.example.com/cb"
        )
        .unwrap_err(),
        InvalidUrl("redirect_uri")
    );
    let local =
        BffConfig::new("http://localhost:8400", "crm", secret(), "http://[::1]/cb").unwrap();
    assert!(!local.allow_loopback_http);
    assert_eq!(local.check(), Err(InvalidUrl("issuer")), "refused at build");
    assert_eq!(local.allow_loopback_http(true).check(), Ok(()));
}

#[test]
fn check_finds_urls_changed_after_reading() {
    let mut c = with(&[]).unwrap();
    assert_eq!(c.check(), Ok(()));
    c.post_logout_redirect_uri = Some("https://evil@crm.example.com/".to_owned());
    assert_eq!(c.check(), Err(InvalidUrl("post_logout_redirect_uri")));
    c.post_logout_redirect_uri = None;
    c.redirect_uri = "http://app.example.com/auth/callback".to_owned();
    assert_eq!(c.check(), Err(InvalidUrl("redirect_uri")));
    assert!(
        !InvalidUrl("redirect_uri")
            .to_string()
            .contains("app.example.com")
    );
}

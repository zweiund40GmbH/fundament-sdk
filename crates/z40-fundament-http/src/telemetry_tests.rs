//! Tests of [`super`].

use super::*;

fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |k| {
        pairs
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| (*v).to_owned())
    }
}

#[test]
fn defaults_and_values() {
    assert_eq!(
        TelemetryConfig::from_lookup("APP", env(&[])).unwrap(),
        TelemetryConfig::default()
    );
    let c = TelemetryConfig::from_lookup(
        "APP",
        env(&[("APP_LOG", " debug,sqlx=warn "), ("APP_LOG_FORMAT", "Text")]),
    )
    .unwrap();
    assert_eq!(c.filter, "debug,sqlx=warn");
    assert_eq!(c.format, LogFormat::Text);
    let empty =
        TelemetryConfig::from_lookup("APP", env(&[("APP_LOG", "  "), ("APP_LOG_FORMAT", "")]));
    assert_eq!(empty.unwrap(), TelemetryConfig::default(), "empty = unset");
    let old = env(&[
        ("RUST_LOG", "warn,sqlx=error"),
        ("APP_LOG_FORMAT", "pretty"),
    ]);
    assert!(
        TelemetryConfig::from_lookup("APP", old).is_err(),
        "only json and text"
    );
}

#[test]
fn rust_log_is_the_fallback() {
    let rust_only = TelemetryConfig::from_lookup("APP", env(&[("RUST_LOG", " warn,sqlx=error ")]));
    assert_eq!(rust_only.unwrap().filter, "warn,sqlx=error");
    let both = env(&[("APP_LOG", "debug"), ("RUST_LOG", "warn")]);
    assert_eq!(
        TelemetryConfig::from_lookup("APP", both).unwrap().filter,
        "debug",
        "own wins"
    );
    let empty_own = env(&[("APP_LOG", " "), ("RUST_LOG", "warn")]);
    assert_eq!(
        TelemetryConfig::from_lookup("APP", empty_own)
            .unwrap()
            .filter,
        "warn"
    );
    let empty_rust = env(&[("RUST_LOG", "")]);
    assert_eq!(
        TelemetryConfig::from_lookup("APP", empty_rust)
            .unwrap()
            .filter,
        "info"
    );
    let bad = TelemetryConfig::from_lookup("APP", env(&[("RUST_LOG", "info,[=x")])).unwrap_err();
    assert_eq!(
        bad.name, "RUST_LOG",
        "the error names the variable that was read"
    );
}

#[test]
fn invalid_values() {
    let bad =
        TelemetryConfig::from_lookup("APP", env(&[("APP_LOG", "info,[=s3cr3t")])).unwrap_err();
    assert_eq!(bad.name, "APP_LOG");
    assert!(!bad.to_string().contains("s3cr3t"));
    let fmt = TelemetryConfig::from_lookup("APP", env(&[("APP_LOG_FORMAT", "xml")])).unwrap_err();
    assert_eq!(fmt.name, "APP_LOG_FORMAT");
    assert_eq!(init(LogFormat::Json, "info,[=x"), Err(InvalidFilter));
}

#[test]
fn init_is_idempotent() {
    // The only test of this binary that touches the global subscriber.
    assert_eq!(
        init(LogFormat::Json, "warn").unwrap(),
        InitOutcome::Installed
    );
    assert_eq!(
        init(LogFormat::Text, "warn").unwrap(),
        InitOutcome::AlreadyInstalled,
        "second call keeps the first subscriber"
    );
}

#[test]
fn app_defaults() {
    let dev = || Defaults {
        filter: "info,sqlx=warn".into(),
        format: LogFormat::Text,
    };
    let c = TelemetryConfig::from_lookup_with("APP", env(&[]), dev()).unwrap();
    assert_eq!(
        (c.filter.as_str(), c.format),
        ("info,sqlx=warn", LogFormat::Text)
    );
    let set = env(&[("APP_LOG", "debug"), ("APP_LOG_FORMAT", "json")]);
    let c = TelemetryConfig::from_lookup_with("APP", set, dev()).unwrap();
    assert_eq!(
        (c.filter.as_str(), c.format),
        ("debug", LogFormat::Json),
        "variables win"
    );
    let rust = env(&[("RUST_LOG", "warn"), ("APP_LOG_FORMAT", " ")]);
    let c = TelemetryConfig::from_lookup_with("APP", rust, dev()).unwrap();
    assert_eq!(
        (c.filter.as_str(), c.format),
        ("warn", LogFormat::Text),
        "RUST_LOG before the default"
    );
    assert_eq!(
        TelemetryConfig::from_lookup_with("APP", env(&[]), Defaults::default()).unwrap(),
        TelemetryConfig::default()
    );
    let bad = Defaults {
        filter: "info,[=s3cr3t".into(),
        format: LogFormat::Json,
    };
    let err = TelemetryConfig::from_lookup_with("APP", env(&[]), bad).unwrap_err();
    assert_eq!(err.name, "APP_LOG");
    assert!(!err.to_string().contains("s3cr3t"), "{err}");
}

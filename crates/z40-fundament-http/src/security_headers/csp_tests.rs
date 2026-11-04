//! Tests of the CSP rules.

use super::*;

const FA: &str = "; frame-ancestors 'none'";

#[test]
fn accepted_policies() {
    let c = check(super::super::API_CSP).unwrap();
    assert_eq!(c.frame_options.unwrap(), "DENY");
    let web = "default-src 'self'; script-src 'self' 'sha256-abc=' 'nonce-r4nd0m' 'strict-dynamic' \
               https://cdn.example.com; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
               frame-ancestors 'self'";
    assert_eq!(check(web).unwrap().frame_options.unwrap(), "SAMEORIGIN");
    let host = "default-src 'self'; frame-ancestors https://example.com";
    assert_eq!(check(host).unwrap().frame_options, None);
    let styles_only =
        "default-src 'self' 'unsafe-inline'; script-src 'self'; frame-ancestors 'none'";
    assert!(
        check(styles_only).is_ok(),
        "script-src replaces default-src for scripts"
    );
    assert!(check("script-src 'none'; frame-ancestors 'none'").is_ok());
}

#[test]
fn unrestricted_or_unframed() {
    let e = |csp: &str| check(csp).unwrap_err();
    assert_eq!(
        e("img-src 'self'; frame-ancestors 'none'"),
        CspError::UnrestrictedScripts
    );
    assert_eq!(e("default-src 'self'"), CspError::NoFrameAncestors);
    assert_eq!(
        e("default-src 'self';\n frame-ancestors 'none'"),
        CspError::InvalidValue
    );
    for open in ["*", "https:", "'self' http:"] {
        let csp = format!("default-src 'self'; frame-ancestors {open}");
        assert_eq!(e(&csp), CspError::OpenFrameAncestors, "{open}");
    }
}

#[test]
fn unsafe_script_sources() {
    let unsafe_sources = [
        "'unsafe-inline'",
        "'UNSAFE-INLINE'",
        "'unsafe-eval'",
        "'unsafe-hashes'",
        "*",
        "data:",
        "http:",
        "https:",
        "blob:",
    ];
    for src in unsafe_sources {
        for directive in [
            "default-src",
            "script-src",
            "script-src-elem",
            "script-src-attr",
        ] {
            let base = if directive == "default-src" {
                ""
            } else {
                "default-src 'self'; "
            };
            let csp = format!("{base}{directive} 'self' {src}{FA}");
            assert_eq!(
                check(&csp).unwrap_err(),
                CspError::UnsafeScriptSource,
                "{csp}"
            );
        }
    }
}

#[test]
fn frame_options_of_any_csp() {
    assert_eq!(frame_options("frame-ancestors 'none'").unwrap(), "DENY");
    assert_eq!(
        frame_options("default-src 'self'; FRAME-ANCESTORS 'self'").unwrap(),
        "SAMEORIGIN"
    );
    assert_eq!(frame_options("frame-ancestors https://example.com"), None);
    assert_eq!(frame_options("default-src 'self'"), None);
}

//! Keeps the generated section of this crate in `docs/configuration.md` in sync with the code (one
//! source of truth). Run `just config-doc` (sets `UPDATE_CONFIG_DOC=1`) to rewrite it.

// The reference covers every feature with settings; compare only when they are enabled.
#![cfg(all(feature = "telemetry", feature = "bff"))]

const BEGIN: &str = "<!-- BEGIN GENERATED: just config-doc (z40-fundament-http) -->\n";
const END: &str = "<!-- END GENERATED (z40-fundament-http) -->";

#[test]
fn configuration_reference_is_current() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/configuration.md");
    let Ok(doc) = std::fs::read_to_string(path) else {
        println!("[skip] docs/configuration.md not found (packaged crate)");
        return;
    };
    let (head, rest) = doc.split_once(BEGIN).expect("begin marker");
    let (_, tail) = rest.split_once(END).expect("end marker");
    let generated = z40_fundament_http::config::reference_markdown();
    let expected = format!("{head}{BEGIN}{generated}{END}{tail}");
    if std::env::var_os("UPDATE_CONFIG_DOC").is_some() {
        std::fs::write(path, &expected).expect("write docs/configuration.md");
        return;
    }
    assert!(
        doc == expected,
        "docs/configuration.md is outdated: run `just config-doc`"
    );
}

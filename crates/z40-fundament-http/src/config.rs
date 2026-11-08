//! The configuration reference of this crate, rendered with the helpers of `z40_fundament::config`
//! (one configuration system for both crates). `docs/configuration.md` contains it; a test keeps it in
//! sync.

use z40_fundament::config::variable_table;

/// The generated part of `docs/configuration.md` for this crate: one table per enabled module that has
/// settings.
pub fn reference_markdown() -> String {
    let mut out = String::new();
    out.push_str("### Client address (`z40-fundament-http`, always on)\n\n");
    out.push_str("Read with `TrustedProxies::from_lookup(prefix, lookup)`.\n\n");
    out.push_str(&variable_table(
        "<PREFIX>",
        crate::client_ip::TrustedProxies::VARIABLES,
    ));
    #[cfg(feature = "telemetry")]
    {
        out.push_str("\n### Log setup (`z40-fundament-http`, feature `telemetry`)\n\n");
        out.push_str("Read with `TelemetryConfig::from_lookup(prefix, lookup)`.\n\n");
        out.push_str(&variable_table(
            "<PREFIX>",
            crate::telemetry::TelemetryConfig::VARIABLES,
        ));
    }
    #[cfg(feature = "bff")]
    {
        out.push_str("\n### Login and sessions (`z40-fundament-http`, feature `bff`)\n\n");
        out.push_str(
            "Read with `BffConfig::from_lookup(prefix, lookup)`. Apps that already use `AUTH_ISSUER`, \
             `AUTH_CLIENT_ID`, … pass the prefix `AUTH`.\n\n",
        );
        out.push_str(&variable_table(
            "<PREFIX>",
            crate::bff::BffConfig::VARIABLES,
        ));
    }
    out
}

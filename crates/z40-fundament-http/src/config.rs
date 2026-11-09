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
        out.push_str(
            "Read with `TelemetryConfig::from_lookup(prefix, lookup)`, or `from_lookup_with(prefix, lookup, \
             Defaults { filter, format })` for the app's own defaults.\n\n",
        );
        out.push_str(&variable_table(
            "<PREFIX>",
            crate::telemetry::TelemetryConfig::VARIABLES,
        ));
    }
    #[cfg(feature = "bff")]
    {
        out.push_str("\n### Login and sessions (`z40-fundament-http`, feature `bff`)\n\n");
        out.push_str(
            "Read with `BffConfig::from_lookup(prefix, lookup)`, or `from_lookup_with(prefix, lookup, \
             Defaults { client_id, redirect_uri, post_logout_redirect_uri, allow_loopback_http })` for \
             defaults the app computes. Apps that already use `AUTH_ISSUER`, `AUTH_CLIENT_ID`, … pass the \
             prefix `AUTH`. Plain-HTTP loopback URLs are refused unless the app allows them \
             (`allow_loopback_http`, development only).\n\n",
        );
        out.push_str(&variable_table(
            "<PREFIX>",
            crate::bff::BffConfig::VARIABLES,
        ));
    }
    out
}

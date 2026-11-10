//! Tests of the limits: defaults and setters.

use super::*;

#[test]
fn defaults_follow_the_protocol() {
    let l = SubgraphLimits::default();
    assert_eq!(l.max_body(), 16_384);
    assert_eq!(l.max_bracket_depth(), 64);
    assert_eq!(l.max_arguments(), 32);
    assert_eq!(l.max_total_arguments(), 1_000);
    assert_eq!(l.max_directives(), 8);
    assert_eq!(l.max_variables(), 50);
    assert_eq!(l.max_name_length(), 256);
    assert_eq!(l.max_errors(), 20);
    assert_eq!(l.max_duration(), Duration::from_secs(10));
    assert_eq!(l.max_fragments(), 30);
    assert_eq!(l.max_spreads(), 100);
    assert_eq!(l.max_selections(), 2_000);
    assert_eq!(l.max_values(), 10_000);
    assert_eq!(l.max_value_bytes(), 1_048_576);
    assert_eq!(l.max_nesting(), 32);
    assert_eq!(l.max_representations(), 100);
    assert_eq!(l.max_reads(), 1_000);
    assert_eq!(l.max_concurrent_reads(), 3);
    assert_eq!(l.max_depth(), 10);
    assert_eq!(l.max_complexity(), 1_000);
}

#[test]
fn setters_override_one_value() {
    let l = SubgraphLimits::default()
        .representations(5)
        .concurrent_reads(0);
    assert_eq!(l.max_representations(), 5);
    assert_eq!(l.max_concurrent_reads(), 1, "0 would block every read");
    assert_eq!(l.max_reads(), 1_000);
}

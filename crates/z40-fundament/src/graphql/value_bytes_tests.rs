//! Tests of the byte count of argument values (strings, enum values, object keys, variables and their
//! defaults) and of the limit on it in the pre-check, through the expansion of fragments.

use super::*;
use crate::graphql::{SubgraphLimits, Violation, precheck};
use async_graphql::parser::parse_query;
use async_graphql::parser::types::Selection;
use serde_json::json;
use std::time::{Duration, Instant};

/// The bytes of the argument values of the first field of the first operation of `q`.
fn first_bytes(q: &str, vars: serde_json::Value) -> usize {
    let doc = parse_query(q).unwrap();
    let vars = Variables::from_json(vars);
    let sizes = variable_sizes(&doc, &vars);
    let (_, op) = doc.operations.iter().next().unwrap();
    let Selection::Field(f) = &op.node.selection_set.node.items[0].node else {
        panic!("not a field")
    };
    argument_values(&f.node.arguments, &sizes)
        .plus(directive_values(&f.node.directives, &sizes))
        .bytes
}

/// The pre-check of `q` with `vars` and `limits`.
fn check_with(q: &str, vars: serde_json::Value, limits: &SubgraphLimits) -> Result<(), Violation> {
    precheck(
        &parse_query(q).unwrap(),
        &Variables::from_json(vars),
        limits,
    )
}

fn check_vars(q: &str, vars: serde_json::Value) -> Result<(), Violation> {
    check_with(q, vars, &SubgraphLimits::default())
}

#[test]
fn literals_count_the_bytes_of_their_texts() {
    assert_eq!(
        first_bytes("{ f(a: 12345, b: true, c: null) }", json!({})),
        0
    );
    // String, enum value, object key and the string inside: 3 + 1 + 3 + 2.
    assert_eq!(
        first_bytes("{ f(a: \"abc\", b: E, c: {key: \"xy\"}) }", json!({})),
        9
    );
    assert_eq!(first_bytes("{ f(a: [\"ä\", \"b\"]) }", json!({})), 3);
    assert_eq!(first_bytes("{ f @include(if: E) }", json!({})), 1);
}

#[test]
fn variables_count_the_bytes_of_their_value_or_default() {
    let q = "query($s: String = \"abc\", $o: In) { f(a: $s, b: $o) }";
    assert_eq!(first_bytes(q, json!({})), 3);
    // A sent value replaces the default; JSON object keys count too.
    assert_eq!(first_bytes(q, json!({ "s": "abcdef" })), 6);
    assert_eq!(
        first_bytes(q, json!({ "o": { "kk": ["x", "yz"] } })),
        3 + 2 + 3
    );
    // Undeclared request variables count their value.
    assert_eq!(
        first_bytes("{ f(a: $u, b: [$u]) }", json!({ "u": "abcd" })),
        8
    );
}

/// The document of the finding: 10 aliases of a field taking `$s` in one fragment spread 100 times –
/// 1 000 field instances, each converting (copying) the value of `$s`.
fn aliases_spread(spreads: usize) -> String {
    let aliases: Vec<String> = (0..10).map(|i| format!("a{i}: caller(a: $s)")).collect();
    format!(
        "query($s: String) {{ {} }} fragment F on Query {{ {} }}",
        "...F ".repeat(spreads),
        aliases.join(" ")
    )
}

#[test]
fn a_long_variable_used_at_many_instances_is_refused_quickly() {
    let q = aliases_spread(100);
    assert!(q.len() < 16 * 1024);
    let doc = parse_query(&q).unwrap();
    let vars = Variables::from_json(json!({ "s": "x".repeat(1_000_000) }));
    let start = Instant::now();
    assert_eq!(
        precheck(&doc, &vars, &SubgraphLimits::default()),
        Err(Violation::ValueBytes)
    );
    assert!(
        start.elapsed() < Duration::from_millis(20),
        "{:?}",
        start.elapsed()
    );
    // The same document with a short value passes (1 000 × 100 bytes).
    assert_eq!(check_vars(&q, json!({ "s": "x".repeat(100) })), Ok(()));
}

#[test]
fn a_record_sized_variable_used_once_passes() {
    let q = "query($s: String) { caller(a: $s) }";
    assert_eq!(
        check_vars(q, json!({ "s": "x".repeat(900 * 1024) })),
        Ok(())
    );
    // Used twice, it exceeds the default of 1 MiB.
    let q = "query($s: String) { a: caller(a: $s) b: caller(a: $s) }";
    assert_eq!(
        check_vars(q, json!({ "s": "x".repeat(900 * 1024) })),
        Err(Violation::ValueBytes)
    );
}

#[test]
fn literals_count_at_every_instance() {
    // A 10 KB literal: written once it passes; 30 × 30 instances are 9 MB.
    let literal = "x".repeat(10_000);
    let spread = |width: usize| {
        format!(
            "{{ {} }} fragment F1 on Query {{ {} }} fragment L on Query {{ caller(a: \"{literal}\") }}",
            "...F1 ".repeat(width),
            "...L ".repeat(width)
        )
    };
    assert_eq!(check_vars(&spread(1), json!({})), Ok(()));
    assert_eq!(
        check_vars(&spread(30), json!({})),
        Err(Violation::ValueBytes)
    );
}

#[test]
fn the_byte_limit_can_be_set() {
    let limits = SubgraphLimits::default().value_bytes(3);
    assert_eq!(check_with("{ a(x: \"abc\") }", json!({}), &limits), Ok(()));
    assert_eq!(
        check_with("{ a(x: \"abc\") b: a(x: \"d\") }", json!({}), &limits),
        Err(Violation::ValueBytes)
    );
    assert_eq!(
        check_with("{ a @skip(if: $v) }", json!({ "v": "abcd" }), &limits),
        Err(Violation::ValueBytes)
    );
}

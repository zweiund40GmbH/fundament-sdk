//! Tests of the value counts (literals, nesting, variables and their defaults) and of the limit on them in
//! the pre-check, through the expansion of fragments.

use super::*;
use crate::graphql::{SubgraphLimits, Violation, precheck};
use async_graphql::parser::parse_query;
use async_graphql::parser::types::Selection;
use serde_json::json;
use std::time::{Duration, Instant};

/// The argument value nodes of the first field of the first operation of `q`.
fn first_field(q: &str, vars: serde_json::Value) -> usize {
    first_size(q, vars).nodes
}

/// The size of the argument values of the first field of the first operation of `q`.
fn first_size(q: &str, vars: serde_json::Value) -> ValueSize {
    let doc = parse_query(q).unwrap();
    let vars = Variables::from_json(vars);
    let sizes = variable_sizes(&doc, &vars);
    let (_, op) = doc.operations.iter().next().unwrap();
    let Selection::Field(f) = &op.node.selection_set.node.items[0].node else {
        panic!("not a field")
    };
    argument_values(&f.node.arguments, &sizes).plus(directive_values(&f.node.directives, &sizes))
}

#[test]
fn literals_count_every_node() {
    assert_eq!(first_field("{ f }", json!({})), 0);
    assert_eq!(
        first_field("{ f(a: 1, b: \"x\", c: null, d: E) }", json!({})),
        4
    );
    assert_eq!(first_field("{ f(a: [1, 2, 3]) }", json!({})), 4);
    assert_eq!(first_field("{ f(a: {x: [1], y: {z: 2}}) }", json!({})), 5);
    assert_eq!(first_field("{ f @include(if: true) }", json!({})), 1);
}

#[test]
fn deep_values_are_counted_without_recursion() {
    let n = 100_000;
    let deep = InputValue::List(vec![]);
    let deep = (0..n).fold(deep, |v, _| InputValue::List(vec![v]));
    assert_eq!(input_size(&deep, &HashMap::new()).nodes, n + 1);
    // Dropping the value would recurse as deep as it nests; leak it instead.
    std::mem::forget(deep);
}

#[test]
fn variables_count_their_value_or_default() {
    let q = "query($v: [Int], $d: [Int] = [1, 2], $n: Int) { f(a: $v, b: $d, c: $n) }";
    assert_eq!(first_field(q, json!({ "v": [1, 2, 3, 4] })), 5 + 3 + 1);
    // A sent value replaces the default.
    assert_eq!(first_field(q, json!({ "d": 7 })), 1 + 1 + 1);
    // Inside a list, a variable counts its own nodes.
    let q = "query($v: [Int]) { f(a: [$v, $v]) }";
    assert_eq!(first_field(q, json!({ "v": [1, 2] })), 1 + 3 + 3);
}

#[test]
fn a_name_declared_twice_counts_its_largest_value() {
    // Whichever operation comes first, its field counts the larger default.
    let q = "query A($v: [Int] = [1]) { f(a: $v) } query B($v: [Int] = [1, 2, 3]) { f(a: $v) }";
    assert_eq!(first_field(q, json!({})), 4);
}

/// The pre-check of `q` with `vars` and `limits`.
fn check_with(q: &str, vars: serde_json::Value, limits: &SubgraphLimits) -> Result<(), Violation> {
    precheck(
        &parse_query(q).unwrap(),
        &Variables::from_json(vars),
        limits,
    )
}

fn check(q: &str) -> Result<(), Violation> {
    check_with(q, json!({}), &SubgraphLimits::default())
}

fn check_vars(q: &str, vars: serde_json::Value) -> Result<(), Violation> {
    check_with(q, vars, &SubgraphLimits::default())
}

/// `fragment L { caller(a: {k0: 1, …}) }` with `keys` keys, `F1` spreading it `width` times and the
/// operation spreading `F1` `width` times: `width²` instances of the literal.
fn spread_literal(keys: usize, width: usize) -> String {
    let object: Vec<String> = (0..keys).map(|i| format!("k{i}:1")).collect();
    format!(
        "{{ {} }} fragment F1 on Query {{ {} }} fragment L on Query {{ caller(a: {{{}}}) }}",
        "...F1 ".repeat(width),
        "...L ".repeat(width),
        object.join(",")
    )
}

#[test]
fn argument_values_add_up_through_the_expansion() {
    let doc = parse_query(spread_literal(6_000, 30)).unwrap();
    let start = Instant::now();
    assert_eq!(
        precheck(&doc, &Variables::default(), &SubgraphLimits::default()),
        Err(Violation::Values)
    );
    assert!(
        start.elapsed() < Duration::from_millis(50),
        "{:?}",
        start.elapsed()
    );
    // The same literal written once passes; spread 2 × 2 times it is 4 × 6 001 nodes.
    assert_eq!(check(&spread_literal(6_000, 1)), Ok(()));
    assert_eq!(check(&spread_literal(6_000, 2)), Err(Violation::Values));
    // An ordinary document with a few literals passes.
    let q = "query($r: [_Any!]! = [{a: 1}]) { a: site(id: \"s-1\", tags: [\"x\", \"y\"]) { name } \
             ...F ...F } fragment F on Query { caller(filter: {kind: A, ids: [1, 2, 3]}) @include(if: true) }";
    assert_eq!(check(q), Ok(()));
}

#[test]
fn referenced_variables_count_at_every_instance() {
    let q = format!(
        "query($v: [Int]) {{ {} }} fragment F1 on Query {{ {} }} fragment L on Query {{ caller(a: $v) }}",
        "...F1 ".repeat(30),
        "...L ".repeat(30)
    );
    let small = json!((0..10).collect::<Vec<_>>());
    assert_eq!(check_vars(&q, json!({ "v": small })), Ok(()));
    let large = json!((0..100).collect::<Vec<_>>());
    assert_eq!(
        check_vars(&q, json!({ "v": large })),
        Err(Violation::Values)
    );
    // Its default counts when the request does not send it.
    let q = q.replace("$v: [Int]", &format!("$v: [Int] = {large}"));
    assert_eq!(check(&q), Err(Violation::Values));
    // A large variable used once is refused too: the engine converts it at that field.
    let huge = json!((0..20_000).collect::<Vec<_>>());
    assert_eq!(
        check_vars("query($v: [Int]) { caller(a: $v) }", json!({ "v": huge })),
        Err(Violation::Values)
    );
}

#[test]
fn the_value_limit_can_be_set() {
    let limits = SubgraphLimits::default().values(3);
    assert_eq!(check_with("{ a(x: [1, 2]) }", json!({}), &limits), Ok(()));
    assert_eq!(
        check_with("{ a(x: [1, 2], y: 3) }", json!({}), &limits),
        Err(Violation::Values)
    );
}

#[test]
fn undeclared_request_variables_count_too() {
    // The engine validates the request value at every argument that uses the variable, declared or not.
    let fields = "f(b: $v) ".repeat(1_000);
    let q = format!("{{ {fields} }}");
    let objects = json!((0..2_700).map(|_| json!({})).collect::<Vec<_>>());
    assert_eq!(
        check_vars(&q, json!({ "v": objects })),
        Err(Violation::Values)
    );
    let directives = format!("{{ {} }}", "x @skip(if: $v) ".repeat(8));
    let large = json!((0..2_000).collect::<Vec<_>>());
    assert_eq!(
        check_vars(&directives, json!({ "v": large })),
        Err(Violation::Values)
    );
    // A small value stays within the limit.
    assert_eq!(check_vars(&q, json!({ "v": 1 })), Ok(()));
    assert_eq!(first_field("{ f(a: $u) }", json!({ "u": [1, 2, 3] })), 4);
}

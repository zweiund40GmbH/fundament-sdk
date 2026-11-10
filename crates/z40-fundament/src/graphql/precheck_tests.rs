//! Tests of the document pre-check: limits, cycles, exponential fan-out and the representation count.

use super::*;
use async_graphql::parser::parse_query;
use serde_json::json;
use std::time::{Duration, Instant};

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

/// `n` fragments `F0 … F(n-1)`, each spreading the next one twice; the operation spreads `F0`.
fn fan_out(n: usize) -> String {
    let mut q = String::from("query { ...F0 }\n");
    for i in 0..n {
        let body = if i + 1 < n {
            format!("a: site {{ name }} ...F{} ...F{}", i + 1, i + 1)
        } else {
            "site { name }".to_string()
        };
        q.push_str(&format!("fragment F{i} on Query {{ {body} }}\n"));
    }
    q
}

/// A field nested `n` levels below the operation (`{ a { a { … name } } }`).
fn nested(n: usize) -> String {
    format!("{{ {}name{} }}", "a { ".repeat(n), " }".repeat(n))
}

/// `n` representations as a JSON list.
fn reps(n: usize) -> serde_json::Value {
    json!(
        (0..n)
            .map(|i| json!({ "__typename": "Site", "id": i.to_string() }))
            .collect::<Vec<_>>()
    )
}

const ENTITIES: &str = "query($r: [_Any!]!) { _entities(representations: $r) { __typename } }";

#[test]
fn ordinary_documents_pass() {
    assert_eq!(check("{ site { name } }"), Ok(()));
    let q = "query { ...A ...A b: site { ...B } } fragment A on Query { site { name } } \
             fragment B on Site { name }";
    assert_eq!(check(q), Ok(()));
    // Spreads of unknown fragments are the engine's business.
    assert_eq!(check("{ ...Unknown }"), Ok(()));
}

#[test]
fn cycles_are_rejected() {
    let own = "query { ...A } fragment A on Query { ...A ...A }";
    assert_eq!(check(own), Err(Violation::Cycle));
    let mutual =
        "query { ...A } fragment A on Query { x: site { name } ...B } fragment B on Query { ...A }";
    assert_eq!(check(mutual), Err(Violation::Cycle));
    // A cycle no operation uses is rejected as unused.
    let unused = "{ site { name } } fragment A on Query { ...B } fragment B on Query { ...A }";
    assert_eq!(check(unused), Err(Violation::UnusedFragment));
}

#[test]
fn fan_out_is_rejected_quickly() {
    let start = Instant::now();
    // 40 levels: more fragments than allowed …
    assert_eq!(check(&fan_out(40)), Err(Violation::Fragments));
    // … and with the fragment and spread limits raised, the chain is too deep (each spread is a level) …
    let many = SubgraphLimits::default().fragments(50);
    assert_eq!(
        check_with(&fan_out(40), json!({}), &many),
        Err(Violation::Nesting)
    );
    // … and with the nesting limit raised too, the expansion stops it.
    let deep = many.nesting(64);
    assert_eq!(
        check_with(&fan_out(40), json!({}), &deep),
        Err(Violation::Selections)
    );
    // 30 levels pass the definition and spread limits (59 spreads) but expand to 2^30 selections.
    assert_eq!(check(&fan_out(30)), Err(Violation::Selections));
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
    // A small fan-out stays allowed: 2^5 copies, well below the limit.
    assert_eq!(check(&fan_out(5)), Ok(()));
}

#[test]
fn spread_and_selection_limits() {
    let q = format!(
        "{{ {}}} fragment A on Query {{ site {{ name }} }}",
        "...A ".repeat(101)
    );
    assert_eq!(check(&q), Err(Violation::Spreads));
    let q = format!(
        "{{ {}}} fragment A on Query {{ site {{ name }} }}",
        "...A ".repeat(100)
    );
    assert_eq!(check(&q), Ok(()));

    // Selections summed over all operations: 3 × 1 000 > 2 000, one of them alone is fine.
    let fields = "site { name } ".repeat(500);
    assert_eq!(check(&format!("query X {{ {fields} }}")), Ok(()));
    let q = format!("query X {{ {fields} }} query Y {{ {fields} }} query Z {{ {fields} }}");
    assert_eq!(check(&q), Err(Violation::Selections));
}

#[test]
fn nesting_counts_like_the_engine() {
    // The operation's set is level 1: 31 levels below it pass, 32 do not.
    assert_eq!(check(&nested(31)), Ok(()));
    assert_eq!(check(&nested(32)), Err(Violation::Nesting));
    assert_eq!(check(&nested(60)), Err(Violation::Nesting));
    // Inline fragments and spreads open a level as well.
    let inline = format!(
        "{{ {}name{} }}",
        "... on Query { ".repeat(32),
        " }".repeat(32)
    );
    assert_eq!(check(&inline), Err(Violation::Nesting));
    let spread_q = |n: usize| {
        let mut q = String::from("query { ...T0 }\n");
        for i in 0..n {
            let inner = if i + 1 < n {
                format!("...T{}", i + 1)
            } else {
                "name".to_string()
            };
            q.push_str(&format!(
                "fragment T{i} on Query {{ a {{ ... on Node {{ {inner} }} }} }}\n"
            ));
        }
        q
    };
    // Each fragment adds three levels (spread, field, inline fragment): 10 → 30, 11 → 33.
    assert_eq!(check(&spread_q(10)), Ok(()));
    assert_eq!(check(&spread_q(11)), Err(Violation::Nesting));
    // A memoised fragment spread again at a deeper level is checked there.
    let deeper = "query { ...D a { a { a { ...D } } } } fragment D on Query { x }";
    let shallow = SubgraphLimits::default().nesting(4);
    assert_eq!(
        check_with(deeper, json!({}), &shallow),
        Err(Violation::Nesting)
    );
}

#[test]
fn representations_from_variables_and_defaults() {
    assert_eq!(check_vars(ENTITIES, json!({ "r": reps(100) })), Ok(()));
    assert_eq!(
        check_vars(ENTITIES, json!({ "r": reps(101) })),
        Err(Violation::Representations)
    );
    // Missing variable: the declared default counts, as the engine uses it.
    let defaulted = format!(
        "query($r: [_Any!]! = {}) {{ _entities(representations: $r) {{ __typename }} }}",
        reps(101)
            .to_string()
            .replace("\"__typename\"", "__typename")
            .replace("\"id\"", "id")
    );
    assert_eq!(check(&defaulted), Err(Violation::Representations));
    // A request value wins over the default.
    assert_eq!(check_vars(&defaulted, json!({ "r": reps(1) })), Ok(()));
    // No value and no default (nullable): null, nothing counted. A single value counts one.
    let nullable = "query($r: [_Any!]) { _entities(representations: $r) { __typename } }";
    assert_eq!(check(nullable), Ok(()));
    assert_eq!(
        check_vars(nullable, json!({ "r": { "__typename": "Site" } })),
        Ok(())
    );
}

#[test]
fn every_occurrence_counts() {
    let r50 = json!({ "r": reps(50), "s": reps(51) });
    // Aliases: 2 × 50 pass, 50 + 51 do not.
    let aliases = "query($r: [_Any!]!, $s: [_Any!]!) { a: _entities(representations: $r) { __typename } \
                   b: _entities(representations: $X) { __typename } }";
    assert_eq!(
        check_vars(&aliases.replace("$X", "$r"), r50.clone()),
        Ok(())
    );
    assert_eq!(
        check_vars(&aliases.replace("$X", "$s"), r50.clone()),
        Err(Violation::Representations)
    );
    // The same fragment spread twice counts twice: 2 × 51.
    let spreads = "query($s: [_Any!]!) { ...F ...F } \
                   fragment F on Query { _entities(representations: $s) { __typename } }";
    assert_eq!(
        check_vars(spreads, r50.clone()),
        Err(Violation::Representations)
    );
    // Inline fragments count as well.
    let inline = "query($r: [_Any!]!, $s: [_Any!]!) { ... on Query { _entities(representations: $r) { __typename } } \
                  _entities(representations: $s) { __typename } }";
    assert_eq!(check_vars(inline, r50), Err(Violation::Representations));
}

#[test]
fn inline_values_and_operations_add_up() {
    // `[$a, $b]` is two entries, whatever the variables hold.
    let two = "query($a: _Any!, $b: _Any!) { _entities(representations: [$a, $b]) { __typename } }";
    let limits = SubgraphLimits::default().representations(2);
    assert_eq!(
        check_with(two, json!({ "a": reps(99), "b": 1 }), &limits),
        Ok(())
    );
    let one = SubgraphLimits::default().representations(1);
    assert_eq!(
        check_with(two, json!({}), &one),
        Err(Violation::Representations)
    );
    // Inline lists by their length, `null` as none, a single object as one.
    let inline = |n: usize| {
        let items = vec!["{ __typename: \"Site\" }"; n].join(", ");
        format!("{{ _entities(representations: [{items}]) {{ __typename }} }}")
    };
    assert_eq!(check(&inline(100)), Ok(()));
    assert_eq!(check(&inline(101)), Err(Violation::Representations));
    assert_eq!(
        check("{ _entities(representations: null) { __typename } }"),
        Ok(())
    );
    let single = "{ _entities(representations: { __typename: \"Site\" }) { __typename } }";
    assert_eq!(check_with(single, json!({}), &one), Ok(()));
    // Summed over all operations, each with its own variables' defaults.
    let ops = "query A($r: [_Any!]!) { _entities(representations: $r) { __typename } } \
               query B($r: [_Any!]!) { _entities(representations: $r) { __typename } }";
    assert_eq!(check_vars(ops, json!({ "r": reps(50) })), Ok(()));
    assert_eq!(
        check_vars(ops, json!({ "r": reps(51) })),
        Err(Violation::Representations)
    );
}

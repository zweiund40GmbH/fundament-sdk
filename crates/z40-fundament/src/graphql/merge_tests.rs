//! Tests of the field-merge count: it matches the engine's rule and multiplies along fragment chains.

use super::super::precheck;
use super::*;
use async_graphql::parser::parse_query;
use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema, Variables};

struct Query;

#[Object]
impl Query {
    async fn a(&self) -> i32 {
        1
    }

    async fn b(&self) -> i32 {
        2
    }

    async fn f(&self, x: Option<i32>, y: Option<i32>) -> i32 {
        x.unwrap_or(0) + y.unwrap_or(0)
    }

    async fn q(&self) -> Query {
        Query
    }
}

/// The conflicts the pre-check counts in `q`: the smallest error limit it passes with.
fn counted(q: &str) -> usize {
    let doc = parse_query(q).unwrap();
    (0..1_000)
        .find(|&n| check_conflicts(&doc, &SubgraphLimits::default().errors(n)).is_ok())
        .unwrap()
}

/// The conflict errors the engine reports for `q`.
async fn reported(q: &str) -> usize {
    let schema = Schema::new(Query, EmptyMutation, EmptySubscription);
    let response = schema.execute(q).await;
    let conflicts = response
        .errors
        .iter()
        .filter(|e| e.message.contains("conflict because"))
        .count();
    assert_eq!(
        conflicts,
        response.errors.len(),
        "only conflicts: {:?}",
        response.errors
    );
    conflicts
}

#[tokio::test]
async fn the_count_matches_the_engine() {
    for q in [
        "{ a a b }",
        "{ x: a x: b x: f(x: 1) x: f(x: 2, y: 3) }",
        "{ x: f(x: 1, y: 2) x: f(y: 2, x: 1) x: f(x: 1) }",
        "{ ...F ...G } fragment F on Query { x: a } fragment G on Query { x: b ...F }",
        "{ ...F2 } fragment F0 on Query { x: a x: b } fragment F1 on Query { ...F0 } \
         fragment F2 on Query { ...F1 x: f }",
        "{ ...A ...B } fragment A on Query { ...C x: a } fragment B on Query { ...C x: b } \
         fragment C on Query { x: f(x: 1) }",
        "{ ... { x: a } ... on Query { x: b ... on Query { x: a } } x: f }",
        "{ q { x: a x: b } q { x: a } q: a }",
        "{ q { ...F } ...F } fragment F on Query { x: f(x: 1) x: f(x: 2) x: f(y: 1) }",
    ] {
        let doc = parse_query(q).unwrap();
        assert_eq!(
            precheck(
                &doc,
                &Variables::default(),
                &SubgraphLimits::default().errors(1_000)
            ),
            Ok(()),
            "{q}"
        );
        assert_eq!(counted(q), reported(q).await, "{q}");
    }
}

#[test]
fn identical_fields_and_other_types_do_not_conflict() {
    let none = SubgraphLimits::default().errors(0);
    for q in [
        "{ id id site { name } site { name } }",
        "{ x: f(x: 1, y: 2) x: f(y: 2, x: 1) }",
        "{ ... on A { x: a } ... on B { x: b } }",
        "{ ...F ...F } fragment F on Query { x: a }",
    ] {
        let doc = parse_query(q).unwrap();
        assert_eq!(check_conflicts(&doc, &none), Ok(()), "{q}");
    }
}

#[test]
fn a_fragment_chain_counts_its_conflicts_once_per_fragment() {
    // Four written sets reach F0 (F0, F1, F2 and the operation), so its one conflict counts four times.
    let q = "{ ...F2 } fragment F0 on Query { x: a x: b } fragment F1 on Query { ...F0 } \
             fragment F2 on Query { ...F1 }";
    assert_eq!(counted(q), 4);
    let doc = parse_query(q).unwrap();
    let limits = SubgraphLimits::default().errors(3);
    assert_eq!(check_conflicts(&doc, &limits), Err(Violation::Conflicts));
}

#[test]
fn each_argument_of_the_first_field_counts() {
    // Different argument count (1) plus the two arguments of the first field the other lacks.
    assert_eq!(counted("{ x: f(x: 1, y: 2) x: f }"), 3);
    // Only the first field's arguments are compared.
    assert_eq!(counted("{ x: f x: f(x: 1, y: 2) }"), 1);
}

/// `fragment F` with the field `first` and then `x: n` 1 998 times, all with the response name `x`.
fn conflicting(first: &str) -> String {
    let args: Vec<String> = (0..32).map(|i| format!("a{i}:1")).collect();
    format!(
        "fragment F on Query {{ x: {first}({}) {} }}",
        args.join(","),
        "x:n ".repeat(1_998)
    )
}

/// The first review document: 29 unused fragments spreading `F`, so the engine's rule walks `F` 30 times
/// and reports ~2 million conflicts, although the operation only asks for `__typename`.
pub(in crate::graphql) fn unused_fan(first: &str) -> String {
    let mut q = format!("{{ __typename }} {}", conflicting(first));
    for i in 0..29 {
        q.push_str(&format!(" fragment G{i} on Query {{ ...F }}"));
    }
    q
}

/// The same conflicts with every fragment used: a chain `F29 → … → F1 → F`, each fragment walked by the
/// rule once for every fragment of the chain above it. 990 fields keep the engine's complexity below its
/// limit, so it would build all errors.
pub(in crate::graphql) fn used_chain() -> String {
    let args: Vec<String> = (0..32).map(|i| format!("a{i}:1")).collect();
    let mut q = format!(
        "{{ ...F29 }} fragment F0 on Query {{ x: m({}) {} }}",
        args.join(","),
        "x:n ".repeat(989)
    );
    for i in 1..30 {
        q.push_str(&format!(" fragment F{i} on Query {{ ...F{} }}", i - 1));
    }
    q
}

#[test]
fn the_review_documents_are_rejected_in_microseconds() {
    let long = "n".repeat(10_000);
    for (q, violation) in [
        (unused_fan("m"), Violation::UnusedFragment),
        (unused_fan(&long), Violation::NameLength),
        (used_chain(), Violation::Conflicts),
    ] {
        let doc = parse_query(&q).unwrap();
        let start = std::time::Instant::now();
        let result = precheck(&doc, &Variables::default(), &SubgraphLimits::default());
        let elapsed = start.elapsed();
        assert_eq!(result, Err(violation));
        let bound = if cfg!(debug_assertions) { 20 } else { 2 };
        assert!(
            elapsed < std::time::Duration::from_millis(bound),
            "{violation}: {elapsed:?}"
        );
    }
}

//! Tests of the written counts: arguments, directives, variables and spreads.

use super::*;
use async_graphql::parser::parse_query;

fn check_with(q: &str, limits: &SubgraphLimits) -> Result<(), Violation> {
    check_written(&parse_query(q).unwrap(), limits)
}

fn check(q: &str) -> Result<(), Violation> {
    check_with(q, &SubgraphLimits::default())
}

/// `{name}: f(a0: 0, a1: 1, …)` with `n` arguments.
fn field(name: &str, n: usize) -> String {
    let args: Vec<String> = (0..n).map(|i| format!("a{i}: {i}")).collect();
    format!("{name}: f({})", args.join(", "))
}

/// `n` directives `@d0 @d1 …`, each with `args` arguments.
fn directives(n: usize, args: usize) -> String {
    let a: Vec<String> = (0..args).map(|i| format!("x{i}: {i}")).collect();
    let a = if args == 0 {
        String::new()
    } else {
        format!("({})", a.join(", "))
    };
    (0..n).map(|i| format!("@d{i}{a} ")).collect()
}

#[test]
fn arguments_per_field_are_bounded() {
    assert_eq!(check(&format!("{{ {} }}", field("x", 32))), Ok(()));
    assert_eq!(
        check(&format!("{{ {} }}", field("x", 33))),
        Err(Violation::Arguments)
    );
    // Also in a fragment no operation uses.
    let q = format!("{{ a }} fragment F on Query {{ {} }}", field("x", 33));
    assert_eq!(check(&q), Err(Violation::Arguments));
}

#[test]
fn arguments_in_total_are_bounded() {
    let fields: Vec<String> = (0..31).map(|i| field(&format!("x{i}"), 32)).collect();
    assert_eq!(check(&format!("{{ {} }}", fields.join(" "))), Ok(()), "992");
    let fields: Vec<String> = (0..32).map(|i| field(&format!("x{i}"), 32)).collect();
    assert_eq!(
        check(&format!("{{ {} }}", fields.join(" "))),
        Err(Violation::TotalArguments),
        "1 024"
    );
    // Directive arguments count too.
    let limits = SubgraphLimits::default().total_arguments(3);
    assert_eq!(
        check_with(&format!("{{ {} @d(a: 1) }}", field("x", 3)), &limits),
        Err(Violation::TotalArguments)
    );
}

#[test]
fn directives_are_bounded_per_location() {
    let ok = directives(8, 1);
    let many = directives(9, 0);
    for q in [
        format!("{{ a {ok} }}"),
        format!("query Q {ok} {{ a }}"),
        format!("query($v: Int {ok}) {{ a }}"),
        format!("{{ ...F {ok} ... on Query {ok} {{ a }} }} fragment F on Query {ok} {{ a }}"),
    ] {
        assert_eq!(check(&q), Ok(()), "{q}");
    }
    for q in [
        format!("{{ a {many} }}"),
        format!("query Q {many} {{ a }}"),
        format!("query($v: Int {many}) {{ a }}"),
        format!("{{ ...F {many} }} fragment F on Query {{ a }}"),
        format!("{{ ... on Query {many} {{ a }} }}"),
        format!("{{ a }} fragment F on Query {many} {{ a }}"),
    ] {
        assert_eq!(check(&q), Err(Violation::Directives), "{q}");
    }
    assert_eq!(
        check(&format!("{{ a {} }}", directives(1, 33))),
        Err(Violation::Arguments)
    );
}

#[test]
fn variable_definitions_are_bounded_per_operation() {
    let vars = |n: usize| -> String { (0..n).map(|i| format!("$v{i}: Int ")).collect() };
    assert_eq!(check(&format!("query({}) {{ a }}", vars(50))), Ok(()));
    assert_eq!(
        check(&format!("query({}) {{ a }}", vars(51))),
        Err(Violation::Variables)
    );
    // Per operation, not per document.
    let two = format!(
        "query A({}) {{ a }} query B({}) {{ a }}",
        vars(50),
        vars(50)
    );
    assert_eq!(check(&two), Ok(()));
}

#[test]
fn spreads_are_counted_as_written() {
    let limits = SubgraphLimits::default().spreads(2);
    assert_eq!(
        check_with("{ ...F ...F } fragment F on Query { a }", &limits),
        Ok(())
    );
    assert_eq!(
        check_with(
            "{ ...F ...F x { ...F } } fragment F on Query { a }",
            &limits
        ),
        Err(Violation::Spreads)
    );
}

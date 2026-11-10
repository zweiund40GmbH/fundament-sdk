//! Tests of the bracket scan: depth, strings, block strings, comments and unbalanced input.

use super::*;
use async_graphql::parser::parse_query;

fn scan(q: &str) -> Result<(), Violation> {
    scan_query(q, &SubgraphLimits::default())
}

#[test]
fn ordinary_documents_pass_and_parse() {
    for q in [
        "{ site { name } }",
        "query($r: [_Any!]! = [{a: [1, 2]}]) { _entities(representations: $r) { __typename } }",
        "{ a(x: \"[[[\", y: \"\\\"{{\") { b } }",
    ] {
        assert_eq!(scan(q), Ok(()), "{q}");
        assert!(parse_query(q).is_ok(), "{q}");
    }
}

#[test]
fn the_limit_counts_brackets_open_at_once() {
    let limits = SubgraphLimits::default().bracket_depth(3);
    assert_eq!(scan_query("{ a(x: 1) }", &limits), Ok(()));
    assert_eq!(scan_query("{ a(x: [1]) }", &limits), Ok(()));
    assert_eq!(
        scan_query("{ a(x: [[1]]) }", &limits),
        Err(Violation::BracketDepth)
    );
    // Siblings close before the next opens.
    assert_eq!(
        scan_query("{ a(x: [1], y: [2]) b(z: [3]) }", &limits),
        Ok(())
    );
}

#[test]
fn deep_values_are_rejected() {
    let lists = format!("{{ f(a: {}1{}) }}", "[".repeat(500), "]".repeat(500));
    assert_eq!(scan(&lists), Err(Violation::BracketDepth));
    let lists = format!("{{ f(a: {}1{}) }}", "[".repeat(3_000), "]".repeat(3_000));
    assert_eq!(scan(&lists), Err(Violation::BracketDepth));
    let objects = format!("{{ f(a: {}1{}) }}", "{a:".repeat(3_000), "}".repeat(3_000));
    assert_eq!(scan(&objects), Err(Violation::BracketDepth));
    let default = format!(
        "query($v: Int = {}1{}) {{ f }}",
        "[".repeat(3_000),
        "]".repeat(3_000)
    );
    assert_eq!(scan(&default), Err(Violation::BracketDepth));
    // Unclosed brackets count as well.
    assert_eq!(scan(&"[".repeat(100)), Err(Violation::BracketDepth));
}

#[test]
fn brackets_in_strings_and_comments_do_not_count() {
    let deep = "[".repeat(3_000);
    for q in [
        format!("{{ f(a: \"{deep}\") }}"),
        format!("{{ f(a: \"\\\"{deep}\") }}"),
        format!("{{ f(a: \"\"\"{deep}\"\"\") }}"),
        format!("{{ f(a: \"\"\" \\\"\"\" {deep} \"\"\") }}"),
        format!("{{ f # {deep}\n }}"),
        format!("# {deep}\r{{ f }}"),
    ] {
        assert_eq!(scan(&q), Ok(()), "{q}");
        assert!(parse_query(&q).is_ok(), "{q}");
    }
}

#[test]
fn a_string_ends_at_a_line_break_like_the_lexer() {
    // The unterminated string does not hide the brackets of the next line.
    let q = format!("{{ f(a: \"open\n{}) }}", "[".repeat(100));
    assert_eq!(scan(&q), Err(Violation::BracketDepth));
}

#[test]
fn unbalanced_input_is_left_to_the_parser() {
    for q in [
        "{ a } }",
        "] ) }",
        "{ a(x: \"unterminated",
        "{ a(x: \"\"\"open",
    ] {
        assert_eq!(scan(q), Ok(()), "{q}");
        assert!(parse_query(q).is_err(), "{q}");
    }
}

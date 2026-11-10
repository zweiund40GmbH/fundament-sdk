//! A property test of the bracket scan against the parser: for random documents with quotes, escapes,
//! comments and brackets in and between their tokens, whenever the parser accepts a document, the scan sees at least as many brackets open
//! at once as the parsed document nests. A scan that skipped text the parser reads (as with an unterminated
//! block string) breaks this.

use super::*;
use async_graphql::parser::parse_query;
use async_graphql::parser::types::{Directive, ExecutableDocument, Selection, SelectionSet};
use async_graphql::{Positioned, Value};
use async_graphql_value::Value as InputValue;

/// Characters of the noise inside values and between tokens: quotes, escapes, comments, line breaks and
/// brackets – the cases where the scan and the parser could disagree about what is text.
const NOISE: &[&str] = &[
    "\"", "\"\"\"", "\\", "\\\"", "\\\"\"\"", "#", "\n", "\r", " ", ",", "[", "]", "{", "}", "a",
    "1",
];

/// A small deterministic generator (xorshift64), so a failure is reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }

    /// Up to `n` noise pieces.
    fn noise(&mut self, q: &mut String, n: usize) {
        for _ in 0..self.next(n + 1) {
            q.push_str(NOISE[self.next(NOISE.len())]);
        }
    }

    /// Whitespace, sometimes a comment with noise, sometimes plain noise.
    fn gap(&mut self, q: &mut String) {
        match self.next(6) {
            0 => {
                q.push('#');
                self.noise(q, 4);
                q.push('\n');
            }
            1 => self.noise(q, 2),
            _ => q.push(' '),
        }
    }

    /// A value: nested lists and objects, scalars, variables and strings with noise inside.
    fn value(&mut self, q: &mut String, depth: usize) {
        match self.next(if depth == 0 { 3 } else { 6 }) {
            0 => q.push('1'),
            1 => {
                q.push_str(["\"", "\"\"\""][self.next(2)]);
                self.noise(q, 4);
                q.push_str(["\"", "\"\"\""][self.next(2)]);
            }
            2 => q.push_str("$a"),
            3 | 4 => {
                q.push('[');
                for _ in 0..self.next(3) {
                    self.value(q, depth - 1);
                    self.gap(q);
                }
                q.push(']');
            }
            _ => {
                q.push_str("{a:");
                self.value(q, depth - 1);
                q.push('}');
            }
        }
    }

    /// A selection set with fields, arguments and nested sets.
    fn set(&mut self, q: &mut String, depth: usize) {
        q.push('{');
        for _ in 0..1 + self.next(2) {
            self.gap(q);
            q.push('a');
            if self.next(2) == 0 {
                q.push_str("(a:");
                self.value(q, 4);
                self.gap(q);
                q.push(')');
            }
            if depth > 0 && self.next(3) == 0 {
                self.set(q, depth - 1);
            }
        }
        self.gap(q);
        q.push('}');
    }
}

/// A random document: an operation with variables, most of them valid, some broken by the noise.
fn document(rng: &mut Rng) -> String {
    let mut q = String::from("query($a: Int");
    if rng.next(3) == 0 {
        q.push_str(" = ");
        rng.value(&mut q, 3);
    }
    q.push(')');
    rng.gap(&mut q);
    rng.set(&mut q, 3);
    q
}

/// Brackets a value opens below itself.
fn value_depth(v: &InputValue) -> usize {
    match v {
        InputValue::List(l) => 1 + l.iter().map(value_depth).max().unwrap_or(0),
        InputValue::Object(o) => 1 + o.values().map(value_depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// Brackets a constant value opens below itself.
fn const_depth(v: &Value) -> usize {
    match v {
        Value::List(l) => 1 + l.iter().map(const_depth).max().unwrap_or(0),
        Value::Object(o) => 1 + o.values().map(const_depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// Brackets the arguments of directives open: `(` plus the deepest value.
fn directives_depth(ds: &[Positioned<Directive>]) -> usize {
    ds.iter()
        .filter(|d| !d.node.arguments.is_empty())
        .map(|d| {
            1 + d
                .node
                .arguments
                .iter()
                .map(|(_, v)| value_depth(&v.node))
                .max()
                .unwrap_or(0)
        })
        .max()
        .unwrap_or(0)
}

/// Brackets a selection set opens: `{` plus the deepest of its selections' arguments and sets.
fn set_depth(set: &SelectionSet) -> usize {
    let inner = set.items.iter().map(|item| match &item.node {
        Selection::Field(f) => {
            let args = f
                .node
                .arguments
                .iter()
                .map(|(_, v)| 1 + value_depth(&v.node))
                .max()
                .unwrap_or(0);
            args.max(directives_depth(&f.node.directives))
                .max(nonempty_depth(&f.node.selection_set.node))
        }
        Selection::InlineFragment(f) => {
            directives_depth(&f.node.directives).max(set_depth(&f.node.selection_set.node))
        }
        Selection::FragmentSpread(s) => directives_depth(&s.node.directives),
    });
    1 + inner.max().unwrap_or(0)
}

/// The set of a field: none (a leaf) opens no bracket.
fn nonempty_depth(set: &SelectionSet) -> usize {
    if set.items.is_empty() {
        0
    } else {
        set_depth(set)
    }
}

/// A lower bound of the brackets open at once in the text of `doc` (variable types are not counted).
fn document_depth(doc: &ExecutableDocument) -> usize {
    let ops = doc.operations.iter().map(|(_, op)| {
        let vars = op.node.variable_definitions.iter().map(|v| {
            let default = v
                .node
                .default_value
                .as_ref()
                .map_or(0, |d| const_depth(&d.node));
            1 + default.max(directives_depth(&v.node.directives))
        });
        vars.max()
            .unwrap_or(0)
            .max(directives_depth(&op.node.directives))
            .max(set_depth(&op.node.selection_set.node))
    });
    let fragments = doc
        .fragments
        .values()
        .map(|f| directives_depth(&f.node.directives).max(set_depth(&f.node.selection_set.node)));
    ops.chain(fragments).max().unwrap_or(0)
}

#[test]
fn the_scan_never_sees_fewer_brackets_than_the_parser() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut parsed = 0usize;
    for _ in 0..20_000 {
        let q = document(&mut rng);
        let Ok(doc) = parse_query(&q) else { continue };
        parsed += 1;
        let depth = document_depth(&doc);
        if depth == 0 {
            continue;
        }
        let limits = SubgraphLimits::default().bracket_depth(depth - 1);
        assert!(scan_query(&q, &limits).is_err(), "{q:?} nests {depth}");
    }
    // The generator must reach the parser often enough for the test to mean something.
    assert!(parsed > 500, "only {parsed} documents parsed");
}

//! Counting the argument values of a document (`docs/protocol.md` §4.2, "Expanded argument values").
//!
//! The engine converts the argument values of every field instance – after expansion, once per spread of
//! the fragment the field sits in – and copies a variable's value into every field that uses it. A large
//! literal in a fragment that is spread many times therefore costs its size at every spread, synchronously.
//! These counts let [`super::precheck`] add the sizes up through the expansion. Every count walks its value
//! with an explicit stack (no recursion, however deep the value).

use async_graphql::parser::types::{Directive, ExecutableDocument};
use async_graphql::{Name, Positioned, Value, Variables};
use async_graphql_value::Value as InputValue;
use std::collections::HashMap;

/// The value nodes of each variable declared in `doc`: of the request's value, else of the declared
/// default, else 1 (`null`). A name declared by several operations gets the largest of its sizes (the
/// engine only uses the executed operation; counting the largest is simple and conservative).
pub(super) fn variable_sizes<'a>(
    doc: &'a ExecutableDocument,
    variables: &Variables,
) -> HashMap<&'a str, usize> {
    let mut sizes = HashMap::new();
    for (_, op) in doc.operations.iter() {
        for def in &op.node.variable_definitions {
            let name = def.node.name.node.as_str();
            let value = variables
                .get(&def.node.name.node)
                .or_else(|| def.node.default_value());
            let size = value.map_or(1, const_nodes);
            let entry = sizes.entry(name).or_insert(0);
            *entry = size.max(*entry);
        }
    }
    sizes
}

/// The value nodes of the arguments of one field or directive, variables counted by `variables`.
pub(super) fn argument_values(
    arguments: &[(Positioned<Name>, Positioned<InputValue>)],
    variables: &HashMap<&str, usize>,
) -> usize {
    arguments.iter().fold(0usize, |n, (_, v)| {
        n.saturating_add(input_nodes(&v.node, variables))
    })
}

/// The value nodes of the arguments of the directives of one location.
pub(super) fn directive_values(
    directives: &[Positioned<Directive>],
    variables: &HashMap<&str, usize>,
) -> usize {
    directives.iter().fold(0usize, |n, d| {
        n.saturating_add(argument_values(&d.node.arguments, variables))
    })
}

/// Nodes of a written value: 1 for each scalar, `null`, enum value, list and object, plus its entries;
/// a variable its size in `variables` (1 if undeclared – the engine rejects it).
fn input_nodes(value: &InputValue, variables: &HashMap<&str, usize>) -> usize {
    let mut n = 0usize;
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        n = n.saturating_add(match v {
            InputValue::Variable(name) => variables.get(name.as_str()).copied().unwrap_or(1),
            InputValue::List(l) => {
                stack.extend(l);
                1
            }
            InputValue::Object(o) => {
                stack.extend(o.values());
                1
            }
            _ => 1,
        });
    }
    n
}

/// Nodes of a constant value, counted like [`input_nodes`].
fn const_nodes(value: &Value) -> usize {
    let mut n = 0usize;
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        n = n.saturating_add(1);
        match v {
            Value::List(l) => stack.extend(l),
            Value::Object(o) => stack.extend(o.values()),
            _ => {}
        }
    }
    n
}

#[cfg(test)]
#[path = "values_tests.rs"]
mod tests;

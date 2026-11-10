//! Counting the argument values of a document (`docs/protocol.md` §4.2, "Expanded argument values" and
//! "Expanded value bytes").
//!
//! The engine converts the argument values of every field instance – after expansion, once per spread of
//! the fragment the field sits in – and copies a variable's value into every field that uses it. A large
//! literal in a fragment that is spread many times therefore costs its size at every spread, synchronously,
//! and so does a single long string variable used at many instances. These counts let [`super::precheck`]
//! add the sizes up through the expansion: the value nodes and the bytes of the texts in them. Every count
//! walks its value by reference with an explicit stack (no recursion however deep the value, no clones).

use async_graphql::parser::types::{Directive, ExecutableDocument};
use async_graphql::{Name, Positioned, Value, Variables};
use async_graphql_value::Value as InputValue;
use std::collections::HashMap;

/// The size of a value: its nodes and the bytes of its texts (strings, enum values, binary values and
/// object keys; numbers, booleans and `null` count 0 bytes – their nodes bound them).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ValueSize {
    /// Each scalar, `null`, enum value, list and object counts 1.
    pub(super) nodes: usize,
    /// Bytes of the texts in the value.
    pub(super) bytes: usize,
}

impl ValueSize {
    /// The size of a value with one node and `bytes` bytes of text.
    const fn node(bytes: usize) -> Self {
        Self { nodes: 1, bytes }
    }

    /// Both sizes added up, saturating.
    pub(super) fn plus(self, other: Self) -> Self {
        Self {
            nodes: self.nodes.saturating_add(other.nodes),
            bytes: self.bytes.saturating_add(other.bytes),
        }
    }

    /// The larger of each of the two sizes.
    fn max(self, other: Self) -> Self {
        Self {
            nodes: self.nodes.max(other.nodes),
            bytes: self.bytes.max(other.bytes),
        }
    }
}

/// One node without text: a list, a number, a boolean, `null` – and a variable neither sent nor declared
/// with a default (it is `null`).
const NO_TEXT: ValueSize = ValueSize::node(0);

/// The size of each variable: every variable of the request with the size of its value (also when no
/// operation declares it – the engine's argument validation still copies the request value into every
/// argument that uses it), and every variable declared in `doc` with the size of the request's value, else
/// of the declared default, else `null`. A name declared by several operations gets the largest of its
/// sizes (the engine only uses the executed operation; counting the largest is simple and conservative).
pub(super) fn variable_sizes<'a>(
    doc: &'a ExecutableDocument,
    variables: &'a Variables,
) -> HashMap<&'a str, ValueSize> {
    let mut sizes: HashMap<&'a str, ValueSize> = variables
        .iter()
        .map(|(name, value)| (name.as_str(), const_size(value)))
        .collect();
    for (_, op) in doc.operations.iter() {
        for def in &op.node.variable_definitions {
            let name = def.node.name.node.as_str();
            let value = variables
                .get(&def.node.name.node)
                .or_else(|| def.node.default_value());
            let size = value.map_or(NO_TEXT, const_size);
            let entry = sizes.entry(name).or_default();
            *entry = size.max(*entry);
        }
    }
    sizes
}

/// The size of the arguments of one field or directive, variables counted by `variables`.
pub(super) fn argument_values(
    arguments: &[(Positioned<Name>, Positioned<InputValue>)],
    variables: &HashMap<&str, ValueSize>,
) -> ValueSize {
    arguments.iter().fold(ValueSize::default(), |n, (_, v)| {
        n.plus(input_size(&v.node, variables))
    })
}

/// The size of the arguments of the directives of one location.
pub(super) fn directive_values(
    directives: &[Positioned<Directive>],
    variables: &HashMap<&str, ValueSize>,
) -> ValueSize {
    directives.iter().fold(ValueSize::default(), |n, d| {
        n.plus(argument_values(&d.node.arguments, variables))
    })
}

/// Size of a written value: 1 node for each scalar, `null`, enum value, list and object, plus its entries,
/// and the bytes of its texts; a variable its size in `variables` (`null` if it is neither in the request
/// nor declared).
fn input_size(value: &InputValue, variables: &HashMap<&str, ValueSize>) -> ValueSize {
    let mut n = ValueSize::default();
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        n = n.plus(match v {
            InputValue::Variable(name) => variables.get(name.as_str()).copied().unwrap_or(NO_TEXT),
            InputValue::List(l) => {
                stack.extend(l);
                NO_TEXT
            }
            InputValue::Object(o) => {
                stack.extend(o.values());
                ValueSize::node(o.keys().fold(0, |b, k| b.saturating_add(k.len())))
            }
            InputValue::String(s) => ValueSize::node(s.len()),
            InputValue::Enum(e) => ValueSize::node(e.len()),
            InputValue::Binary(b) => ValueSize::node(b.len()),
            InputValue::Null | InputValue::Number(_) | InputValue::Boolean(_) => NO_TEXT,
        });
    }
    n
}

/// Size of a constant value, counted like [`input_size`].
fn const_size(value: &Value) -> ValueSize {
    let mut n = ValueSize::default();
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        n = n.plus(match v {
            Value::List(l) => {
                stack.extend(l);
                NO_TEXT
            }
            Value::Object(o) => {
                stack.extend(o.values());
                ValueSize::node(o.keys().fold(0, |b, k| b.saturating_add(k.len())))
            }
            Value::String(s) => ValueSize::node(s.len()),
            Value::Enum(e) => ValueSize::node(e.len()),
            Value::Binary(b) => ValueSize::node(b.len()),
            Value::Null | Value::Number(_) | Value::Boolean(_) => NO_TEXT,
        });
    }
    n
}

#[cfg(test)]
#[path = "values_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "value_bytes_tests.rs"]
mod bytes_tests;

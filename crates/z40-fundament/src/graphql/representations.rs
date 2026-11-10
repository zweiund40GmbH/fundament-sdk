//! Counting the `_entities` representations of a document (`docs/protocol.md` §4.2). Every occurrence
//! counts – each alias, inline fragment and spread – because the engine executes every occurrence of a
//! field separately, even where they merge into one response field. Sum over all operations: a request
//! asking for more in an operation it does not run is refused as well (simple and conservative).
//!
//! Runs only on a document that passed the shape checks of [`super::precheck`] (acyclic, bounded nesting),
//! so the recursion is bounded. A fragment's count is computed once per operation (the variables differ
//! between operations) and added at every spread. The work is bounded rather than strictly linear: an
//! operation walks its own selections and, once each, the fragments it reaches – only operations with a
//! spread reach any – so at most (written spreads + 1) × the size of the document.

use async_graphql::parser::types::{
    ExecutableDocument, Field, OperationDefinition, Selection, SelectionSet,
};
use async_graphql::{Name, Value, Variables};
use async_graphql_value::Value as InputValue;
use std::collections::HashMap;

/// The representations of all operations of `doc`; stops early once the sum exceeds `cap` (the exact
/// excess does not matter).
pub(super) fn count_representations(
    doc: &ExecutableDocument,
    variables: &Variables,
    cap: usize,
) -> usize {
    let mut total = 0usize;
    for (_, op) in doc.operations.iter() {
        let mut c = Count {
            doc,
            op: &op.node,
            variables,
            cap,
            memo: HashMap::new(),
        };
        total = total.saturating_add(c.set(&op.node.selection_set.node));
        if total > cap {
            break;
        }
    }
    total
}

/// The count of one operation: its variables and the memo of fragment counts.
struct Count<'a> {
    doc: &'a ExecutableDocument,
    op: &'a OperationDefinition,
    variables: &'a Variables,
    cap: usize,
    memo: HashMap<&'a str, usize>,
}

impl<'a> Count<'a> {
    /// The representations below `set`.
    fn set(&mut self, set: &'a SelectionSet) -> usize {
        let mut sum = 0usize;
        for item in &set.items {
            let n = match &item.node {
                Selection::Field(f) => {
                    let own = if f.node.name.node == "_entities" {
                        self.argument(&f.node)
                    } else {
                        0
                    };
                    own.saturating_add(self.set(&f.node.selection_set.node))
                }
                Selection::InlineFragment(i) => self.set(&i.node.selection_set.node),
                Selection::FragmentSpread(s) => self.fragment(s.node.fragment_name.node.as_str()),
            };
            sum = sum.saturating_add(n);
            if sum > self.cap {
                break;
            }
        }
        sum
    }

    /// The representations of fragment `name` (0 if unknown), memoised. A fragment still being counted
    /// (only in a cyclic document, which the shape check rejects first) counts 0.
    fn fragment(&mut self, name: &'a str) -> usize {
        if let Some(n) = self.memo.get(name) {
            return *n;
        }
        let Some(def) = self.doc.fragments.get(name) else {
            return 0;
        };
        self.memo.insert(name, 0);
        let n = self.set(&def.node.selection_set.node);
        self.memo.insert(name, n);
        n
    }

    /// Entries of the `representations` argument of `field`: an inline list by its length (`[$a, $b]` is
    /// 2), a variable by its value, `null` 0, anything else 1 (list coercion).
    fn argument(&self, field: &Field) -> usize {
        match field.get_argument("representations").map(|a| &a.node) {
            None | Some(InputValue::Null) => 0,
            Some(InputValue::Variable(name)) => self.variable(name).map_or(0, entries),
            Some(InputValue::List(l)) => l.len(),
            Some(_) => 1,
        }
    }

    /// The value of variable `name`, borrowed: from the request, else the default declared in the
    /// operation (the engine falls back to it too; a nullable variable without default is `null`), else
    /// none.
    fn variable(&self, name: &Name) -> Option<&'a Value> {
        self.variables.get(name).or_else(|| {
            self.op
                .variable_definitions
                .iter()
                .find(|d| d.node.name.node == *name)
                .and_then(|d| d.node.default_value())
        })
    }
}

/// Entries of a resolved value: a list its length, `null` none, anything else one.
fn entries(v: &Value) -> usize {
    match v {
        Value::List(l) => l.len(),
        Value::Null => 0,
        _ => 1,
    }
}

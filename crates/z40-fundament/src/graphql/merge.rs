//! The field-merge count of the pre-check (`docs/protocol.md` §4.2, "Field-merge conflicts").
//!
//! The engine's rule for fields with the same response name runs once for **every written selection set**
//! – operations, fields, inline fragments and fragment definitions alike – and walks that set together with
//! the inline fragments and fragment spreads in it (each fragment once per walk). A fragment spread through
//! a chain of other fragments is therefore walked once for each fragment of the chain, not once per spread
//! as the expansion counts it, and the rule reports every conflict it meets on each walk: one error when
//! the fields differ, one when their argument counts differ and one for each argument of the first field
//! the other does not match. The messages are built in full before the engine checks anything else.
//!
//! This module repeats exactly that walk and count, without building messages, and stops as soon as the
//! count passes the error limit. Its work is that of the engine's walk, which the expansion limits bound
//! (every set is reachable, see [`super::precheck`]). Explicit stacks: no recursion.

use super::{SubgraphLimits, Violation};
use async_graphql::Positioned;
use async_graphql::parser::types::{ExecutableDocument, Field, Selection, SelectionSet};
use std::collections::{HashMap, HashSet};

/// Rejects `doc` if the engine's field-merge rule would report more than
/// [`SubgraphLimits::max_errors`] conflicts over all written selection sets.
pub(super) fn check_conflicts(
    doc: &ExecutableDocument,
    limits: &SubgraphLimits,
) -> Result<(), Violation> {
    let mut count = Count {
        doc,
        conflicts: 0,
        cap: limits.max_errors(),
    };
    let mut sets: Vec<&SelectionSet> = Vec::new();
    for (_, op) in doc.operations.iter() {
        sets.push(&op.node.selection_set.node);
    }
    for def in doc.fragments.values() {
        sets.push(&def.node.selection_set.node);
    }
    while let Some(set) = sets.pop() {
        for item in &set.items {
            match &item.node {
                Selection::Field(f) => sets.push(&f.node.selection_set.node),
                Selection::InlineFragment(f) => sets.push(&f.node.selection_set.node),
                Selection::FragmentSpread(_) => {}
            }
        }
        count.walk(set)?;
    }
    Ok(())
}

/// The conflicts counted so far and the most allowed.
struct Count<'a> {
    doc: &'a ExecutableDocument,
    conflicts: usize,
    cap: usize,
}

/// The items of one set still to walk, with the type condition they are under.
type Frame<'a> = (Option<&'a str>, std::slice::Iter<'a, Positioned<Selection>>);

impl<'a> Count<'a> {
    /// One walk of the engine's rule over `set`: its fields and those of its inline fragments and spreads
    /// (each fragment once), keyed by type condition and response name, in the engine's order.
    fn walk(&mut self, set: &'a SelectionSet) -> Result<(), Violation> {
        let mut first: HashMap<(Option<&'a str>, &'a str), &'a Field> = HashMap::new();
        let mut visited: HashSet<&'a str> = HashSet::new();
        let mut stack: Vec<Frame<'a>> = vec![(None, set.items.iter())];
        loop {
            let Some((on_type, items)) = stack.last_mut() else {
                return Ok(());
            };
            let on_type = *on_type;
            let Some(item) = items.next() else {
                stack.pop();
                continue;
            };
            match &item.node {
                Selection::Field(f) => {
                    let f = &f.node;
                    let key = (on_type, f.alias.as_ref().unwrap_or(&f.name).node.as_str());
                    match first.get(&key) {
                        Some(prev) => self.add(conflicts(prev, f))?,
                        None => {
                            first.insert(key, f);
                        }
                    }
                }
                Selection::InlineFragment(f) => {
                    let on = f.node.type_condition.as_ref();
                    let on = on.map(|c| c.node.on.node.as_str());
                    stack.push((on, f.node.selection_set.node.items.iter()));
                }
                Selection::FragmentSpread(s) => {
                    let name = s.node.fragment_name.node.as_str();
                    if let Some(def) = self.doc.fragments.get(name)
                        && visited.insert(name)
                    {
                        let on = Some(def.node.type_condition.node.on.node.as_str());
                        stack.push((on, def.node.selection_set.node.items.iter()));
                    }
                }
            }
        }
    }

    /// Adds `n` conflicts; more than the cap → [`Violation::Conflicts`].
    fn add(&mut self, n: usize) -> Result<(), Violation> {
        self.conflicts = self.conflicts.saturating_add(n);
        if self.conflicts > self.cap {
            return Err(Violation::Conflicts);
        }
        Ok(())
    }
}

/// The errors the engine reports for `field` after `prev` with the same response name: different
/// fields, different argument counts, and each argument of `prev` that `field` does not have equally.
fn conflicts(prev: &Field, field: &Field) -> usize {
    let mut n = usize::from(prev.name.node != field.name.node);
    n += usize::from(prev.arguments.len() != field.arguments.len());
    for (name, value) in &prev.arguments {
        if field.get_argument(&name.node) != Some(value) {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
#[path = "merge_tests.rs"]
pub(super) mod tests;

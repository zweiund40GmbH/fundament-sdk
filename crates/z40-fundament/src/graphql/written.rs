//! The limits of the written document (`docs/protocol.md` §4.2), before anything is expanded: fragment
//! spreads, arguments per field and directive and in total, directives per location and variable
//! definitions per operation.
//!
//! The engine compares the arguments of every pair of fields with the same response name (quadratic in the
//! arguments of a field) and walks directives per location; these counts keep that work small. One walk
//! with an explicit stack (no recursion, however deep the document), stopping at the first broken limit.

use super::{SubgraphLimits, Violation};
use async_graphql::Positioned;
use async_graphql::parser::types::{Directive, ExecutableDocument, Selection, SelectionSet};

/// Checks the written counts of `doc` against `limits`.
pub(super) fn check_written(
    doc: &ExecutableDocument,
    limits: &SubgraphLimits,
) -> Result<(), Violation> {
    let mut w = Written {
        limits,
        spreads: 0,
        arguments: 0,
    };
    let mut stack: Vec<&SelectionSet> = Vec::new();
    for (_, op) in doc.operations.iter() {
        let op = &op.node;
        if op.variable_definitions.len() > limits.max_variables() {
            return Err(Violation::Variables);
        }
        w.directives(&op.directives)?;
        for var in &op.variable_definitions {
            w.directives(&var.node.directives)?;
        }
        stack.push(&op.selection_set.node);
    }
    for def in doc.fragments.values() {
        w.directives(&def.node.directives)?;
        stack.push(&def.node.selection_set.node);
    }
    while let Some(set) = stack.pop() {
        for item in &set.items {
            match &item.node {
                Selection::Field(f) => {
                    w.arguments(f.node.arguments.len())?;
                    w.directives(&f.node.directives)?;
                    stack.push(&f.node.selection_set.node);
                }
                Selection::InlineFragment(f) => {
                    w.directives(&f.node.directives)?;
                    stack.push(&f.node.selection_set.node);
                }
                Selection::FragmentSpread(s) => {
                    w.spreads += 1;
                    if w.spreads > limits.max_spreads() {
                        return Err(Violation::Spreads);
                    }
                    w.directives(&s.node.directives)?;
                }
            }
        }
    }
    Ok(())
}

/// The counts of the walk so far.
struct Written<'a> {
    limits: &'a SubgraphLimits,
    spreads: usize,
    arguments: usize,
}

impl Written<'_> {
    /// Counts `n` arguments of one field or directive.
    fn arguments(&mut self, n: usize) -> Result<(), Violation> {
        if n > self.limits.max_arguments() {
            return Err(Violation::Arguments);
        }
        self.arguments = self.arguments.saturating_add(n);
        if self.arguments > self.limits.max_total_arguments() {
            return Err(Violation::TotalArguments);
        }
        Ok(())
    }

    /// Counts the directives of one location and their arguments.
    fn directives(&mut self, directives: &[Positioned<Directive>]) -> Result<(), Violation> {
        if directives.len() > self.limits.max_directives() {
            return Err(Violation::Directives);
        }
        for d in directives {
            self.arguments(d.node.arguments.len())?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "written_tests.rs"]
mod tests;

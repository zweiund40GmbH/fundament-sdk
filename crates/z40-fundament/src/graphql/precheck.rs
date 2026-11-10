//! The document pre-check of a subgraph (`docs/protocol.md` §4.2), run on the parsed document **before**
//! the engine sees it.
//!
//! async-graphql walks fragment spreads by expanding them (its recursion-depth check before validation,
//! the complexity and depth visitors), so a few fragments that each spread the next one twice cost 2^n
//! steps – synchronously, on a runtime thread. This check is linear in the size of the document: each
//! fragment is measured once (memo by name), and documents whose expansion would be large, deep or cyclic
//! are rejected, so every later walk over the expanded document is cheap. Its own recursion never goes
//! deeper than the nesting limit.

use super::SubgraphLimits;
use super::representations::count_representations;
use async_graphql::Variables;
use async_graphql::parser::types::{ExecutableDocument, Selection, SelectionSet};
use std::collections::{HashMap, HashSet};

/// Which limit a document broke. For logs only: the client gets the generic message of
/// `docs/protocol.md` §4.5, never this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Violation {
    /// More fragment definitions than [`SubgraphLimits::max_fragments`].
    #[error("too many fragment definitions")]
    Fragments,
    /// More fragment spreads than [`SubgraphLimits::max_spreads`].
    #[error("too many fragment spreads")]
    Spreads,
    /// A fragment spreads itself, directly or through others (also in unused fragments).
    #[error("fragment cycle")]
    Cycle,
    /// More selections after expansion than [`SubgraphLimits::max_selections`].
    #[error("too many selections after expanding fragments")]
    Selections,
    /// Nesting deeper than [`SubgraphLimits::max_nesting`] after expansion.
    #[error("selections nested too deep")]
    Nesting,
    /// More `_entities` representations than [`SubgraphLimits::max_representations`].
    #[error("too many _entities representations")]
    Representations,
}

/// Checks `doc` with the request's `variables` against `limits` (`docs/protocol.md` §4.2). `Ok` means
/// the document is acyclic and, with every spread expanded, small and shallow enough, and asks for at most
/// the allowed number of representations. Spreads of unknown fragments are left to the engine's
/// validation. Pure: no IO, nothing is cloned from `variables`.
pub fn precheck(
    doc: &ExecutableDocument,
    variables: &Variables,
    limits: &SubgraphLimits,
) -> Result<(), Violation> {
    if doc.fragments.len() > limits.max_fragments() {
        return Err(Violation::Fragments);
    }
    let sets = doc
        .operations
        .iter()
        .map(|(_, op)| &op.node.selection_set.node)
        .chain(doc.fragments.values().map(|f| &f.node.selection_set.node));
    if count_spreads(sets, limits.max_spreads()) > limits.max_spreads() {
        return Err(Violation::Spreads);
    }
    let mut m = Measure {
        doc,
        limits,
        done: HashMap::new(),
        open: HashSet::new(),
    };
    let mut total = 0usize;
    for (_, op) in doc.operations.iter() {
        total = total.saturating_add(m.set(&op.node.selection_set.node, 1)?.selections);
        if total > limits.max_selections() {
            return Err(Violation::Selections);
        }
    }
    // Unused fragments too: the engine validates every definition, cycles there must not reach it.
    for name in doc.fragments.keys() {
        m.fragment(name, 1)?;
    }
    // Only now: the counting expands spreads and relies on an acyclic, bounded document.
    if count_representations(doc, variables, limits.max_representations())
        > limits.max_representations()
    {
        return Err(Violation::Representations);
    }
    Ok(())
}

/// Fragment spreads written in `sets` (not following them), counted with an explicit stack (no
/// recursion, however deep the document) and stopping once the count exceeds `cap`.
fn count_spreads<'a>(sets: impl Iterator<Item = &'a SelectionSet>, cap: usize) -> usize {
    let mut stack: Vec<&SelectionSet> = sets.collect();
    let mut n = 0usize;
    while let Some(set) = stack.pop() {
        for item in &set.items {
            match &item.node {
                Selection::Field(f) => stack.push(&f.node.selection_set.node),
                Selection::InlineFragment(f) => stack.push(&f.node.selection_set.node),
                Selection::FragmentSpread(_) => n = n.saturating_add(1),
            }
        }
        if n > cap {
            break;
        }
    }
    n
}

/// Size of an expanded selection set: its selections (recursively) and how many levels of selection sets
/// it spans (1 for a set of leaf fields, 0 for no set).
#[derive(Debug, Clone, Copy)]
struct Size {
    selections: usize,
    levels: usize,
}

/// No selection set below a leaf field (and an unknown fragment).
const EMPTY: Size = Size {
    selections: 0,
    levels: 0,
};

/// One measurement of a document: fragments measured so far (memo) and those being measured (cycles).
struct Measure<'a> {
    doc: &'a ExecutableDocument,
    limits: &'a SubgraphLimits,
    done: HashMap<&'a str, Size>,
    open: HashSet<&'a str>,
}

impl<'a> Measure<'a> {
    /// Whether a set at `level` spanning `levels` levels reaches deeper than the nesting limit.
    fn too_deep(&self, level: usize, levels: usize) -> bool {
        level.saturating_add(levels) > self.limits.max_nesting().saturating_add(1)
    }

    /// Measures `set` at nesting `level` (the operation's own set is level 1; every field with a set,
    /// inline fragment and spread opens the next level, as the engine counts). The recursion never goes
    /// deeper than the nesting limit + 1 frames, and each fragment body is walked once.
    fn set(&mut self, set: &'a SelectionSet, level: usize) -> Result<Size, Violation> {
        if level > self.limits.max_nesting() {
            return Err(Violation::Nesting);
        }
        let mut size = Size {
            selections: 0,
            levels: 1,
        };
        for item in &set.items {
            let below = match &item.node {
                Selection::Field(f) => self.below(&f.node.selection_set.node, level)?,
                Selection::InlineFragment(f) => self.below(&f.node.selection_set.node, level)?,
                Selection::FragmentSpread(s) => {
                    self.fragment(s.node.fragment_name.node.as_str(), level + 1)?
                }
            };
            size.selections = size
                .selections
                .saturating_add(1)
                .saturating_add(below.selections);
            size.levels = size.levels.max(below.levels.saturating_add(1));
            if size.selections > self.limits.max_selections() {
                return Err(Violation::Selections);
            }
            if self.too_deep(level, size.levels) {
                return Err(Violation::Nesting);
            }
        }
        Ok(size)
    }

    /// Measures the set below a field or inline fragment at `level` (empty for a leaf field).
    fn below(&mut self, set: &'a SelectionSet, level: usize) -> Result<Size, Violation> {
        if set.items.is_empty() {
            return Ok(EMPTY);
        }
        self.set(set, level + 1)
    }

    /// Measures fragment `name` whose body sits at `level` (once; later spreads reuse the result and
    /// only check the depth at their own level). Spreading a fragment still being measured is a cycle.
    fn fragment(&mut self, name: &'a str, level: usize) -> Result<Size, Violation> {
        if let Some(size) = self.done.get(name) {
            if self.too_deep(level, size.levels) {
                return Err(Violation::Nesting);
            }
            return Ok(*size);
        }
        let Some(def) = self.doc.fragments.get(name) else {
            return Ok(EMPTY);
        };
        if !self.open.insert(name) {
            return Err(Violation::Cycle);
        }
        let size = self.set(&def.node.selection_set.node, level)?;
        self.open.remove(name);
        self.done.insert(name, size);
        Ok(size)
    }
}

#[cfg(test)]
#[path = "precheck_tests.rs"]
mod tests;

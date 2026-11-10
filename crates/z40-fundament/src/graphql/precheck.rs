//! The document pre-check of a subgraph (`docs/protocol.md` §4.2), run on the parsed document **before**
//! the engine sees it.
//!
//! async-graphql walks fragment spreads by expanding them (its recursion-depth check before validation,
//! the complexity and depth visitors), so a few fragments that each spread the next one twice cost 2^n
//! steps – synchronously, on a runtime thread. This check is linear in the size of the document: each
//! fragment is measured once (memo by name), and documents whose expansion would be large, deep or cyclic
//! are rejected, so every later walk over the expanded document is cheap. The argument values are added up
//! through the expansion the same way, because the engine converts them at every field instance. Its own
//! recursion never goes deeper than the nesting limit.
//!
//! Every fragment must be used: the engine validates unused fragments as well, and they would be walked
//! without being counted. So each fragment body counts toward the totals through its spreads. The
//! engine's field-merge rule walks fragments per written selection set instead of per spread; its
//! conflicts are counted separately ([`super::merge`]).

use super::SubgraphLimits;
use super::merge::check_conflicts;
use super::representations::count_representations;
use super::values::{ValueSize, argument_values, directive_values, variable_sizes};
use super::written::check_written;
use async_graphql::Variables;
use async_graphql::parser::types::{ExecutableDocument, Selection, SelectionSet};
use std::collections::{HashMap, HashSet};

/// Which limit a document broke. For logs only: the client gets the generic message of
/// `docs/protocol.md` §4.5, never this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Violation {
    /// More brackets open at once in the raw query than [`SubgraphLimits::max_bracket_depth`]
    /// ([`super::scan_query`]).
    #[error("brackets nested too deep")]
    BracketDepth,
    /// A block string `"""` without its closing `"""` in the raw query ([`super::scan_query`]): the parser
    /// would read its quotes as an empty string and parse the text after it.
    #[error("unterminated block string")]
    UnterminatedString,
    /// More arguments on one field or directive than [`SubgraphLimits::max_arguments`].
    #[error("too many arguments on one field or directive")]
    Arguments,
    /// More arguments in the document than [`SubgraphLimits::max_total_arguments`].
    #[error("too many arguments")]
    TotalArguments,
    /// More directives on one location than [`SubgraphLimits::max_directives`].
    #[error("too many directives on one location")]
    Directives,
    /// More variable definitions in one operation than [`SubgraphLimits::max_variables`].
    #[error("too many variable definitions")]
    Variables,
    /// More fragment definitions than [`SubgraphLimits::max_fragments`].
    #[error("too many fragment definitions")]
    Fragments,
    /// More fragment spreads than [`SubgraphLimits::max_spreads`].
    #[error("too many fragment spreads")]
    Spreads,
    /// A fragment spreads itself, directly or through others.
    #[error("fragment cycle")]
    Cycle,
    /// A fragment definition no operation reaches through its spreads.
    #[error("unused fragment")]
    UnusedFragment,
    /// A name longer than [`SubgraphLimits::max_name_length`] bytes.
    #[error("name too long")]
    NameLength,
    /// More conflicts of fields with the same response name than [`SubgraphLimits::max_errors`], counted
    /// as the engine reports them.
    #[error("too many field-merge conflicts")]
    Conflicts,
    /// More selections after expansion than [`SubgraphLimits::max_selections`].
    #[error("too many selections after expanding fragments")]
    Selections,
    /// More argument values after expansion than [`SubgraphLimits::max_values`].
    #[error("too many argument values after expanding fragments")]
    Values,
    /// More bytes of argument values after expansion than [`SubgraphLimits::max_value_bytes`].
    #[error("argument values too large after expanding fragments")]
    ValueBytes,
    /// Nesting deeper than [`SubgraphLimits::max_nesting`] after expansion.
    #[error("selections nested too deep")]
    Nesting,
    /// More `_entities` representations than [`SubgraphLimits::max_representations`].
    #[error("too many _entities representations")]
    Representations,
}

/// Checks `doc` with the request's `variables` against `limits` (`docs/protocol.md` §4.2). `Ok` means
/// the written document has few enough spreads, arguments, directives and variables and only short names,
/// is acyclic, uses every fragment and, with every spread expanded, is small and shallow enough with few
/// and short enough argument values, has few enough field-merge conflicts and asks for at most the allowed number of
/// representations. Run [`super::scan_query`] on the query text before parsing it.
/// Spreads of unknown fragments are left to the engine's validation. Pure: no IO, nothing is cloned from
/// `variables`.
pub fn precheck(
    doc: &ExecutableDocument,
    variables: &Variables,
    limits: &SubgraphLimits,
) -> Result<(), Violation> {
    if doc.fragments.len() > limits.max_fragments() {
        return Err(Violation::Fragments);
    }
    check_written(doc, limits)?;
    let mut m = Measure {
        doc,
        limits,
        variables: variable_sizes(doc, variables),
        done: HashMap::new(),
        open: HashSet::new(),
    };
    let mut total = EMPTY;
    for (_, op) in doc.operations.iter() {
        let size = m.set(&op.node.selection_set.node, 1)?;
        total.selections = total.selections.saturating_add(size.selections);
        total.values = total.values.plus(size.values);
        m.check(total)?;
    }
    // A fragment not measured through an operation is unused: the engine would walk it uncounted.
    if doc
        .fragments
        .keys()
        .any(|name| !m.done.contains_key(name.as_str()))
    {
        return Err(Violation::UnusedFragment);
    }
    // Only now: both walks below rely on an acyclic, bounded document whose fragments are all counted.
    check_conflicts(doc, limits)?;
    if count_representations(doc, variables, limits.max_representations())
        > limits.max_representations()
    {
        return Err(Violation::Representations);
    }
    Ok(())
}

/// Size of an expanded selection set: its selections (recursively), the size of their argument values
/// and how many levels of selection sets it spans (1 for a set of leaf fields, 0 for no set).
#[derive(Debug, Clone, Copy)]
struct Size {
    selections: usize,
    values: ValueSize,
    levels: usize,
}

/// No selection set below a leaf field (and an unknown fragment).
const EMPTY: Size = Size {
    selections: 0,
    values: ValueSize { nodes: 0, bytes: 0 },
    levels: 0,
};

/// One measurement of a document: the sizes of its variables, fragments measured so far (memo) and
/// those being measured (cycles).
struct Measure<'a> {
    doc: &'a ExecutableDocument,
    limits: &'a SubgraphLimits,
    variables: HashMap<&'a str, ValueSize>,
    done: HashMap<&'a str, Size>,
    open: HashSet<&'a str>,
}

impl<'a> Measure<'a> {
    /// Whether `size` stays within the limits of selections and argument values.
    fn check(&self, size: Size) -> Result<(), Violation> {
        if size.selections > self.limits.max_selections() {
            return Err(Violation::Selections);
        }
        if size.values.nodes > self.limits.max_values() {
            return Err(Violation::Values);
        }
        if size.values.bytes > self.limits.max_value_bytes() {
            return Err(Violation::ValueBytes);
        }
        Ok(())
    }

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
        let mut size = Size { levels: 1, ..EMPTY };
        for item in &set.items {
            let vars = &self.variables;
            let (own, below) = match &item.node {
                Selection::Field(f) => (
                    argument_values(&f.node.arguments, vars)
                        .plus(directive_values(&f.node.directives, vars)),
                    self.below(&f.node.selection_set.node, level)?,
                ),
                Selection::InlineFragment(f) => (
                    directive_values(&f.node.directives, vars),
                    self.below(&f.node.selection_set.node, level)?,
                ),
                Selection::FragmentSpread(s) => (
                    directive_values(&s.node.directives, vars),
                    self.fragment(s.node.fragment_name.node.as_str(), level + 1)?,
                ),
            };
            size.selections = size
                .selections
                .saturating_add(1)
                .saturating_add(below.selections);
            size.values = size.values.plus(own).plus(below.values);
            size.levels = size.levels.max(below.levels.saturating_add(1));
            self.check(size)?;
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

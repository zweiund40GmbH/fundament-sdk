//! The limits of a subgraph (`docs/protocol.md` §4): what the document pre-check, the endpoint and the
//! read budget enforce, and the engine limits [`SubgraphLimits::apply`] sets on the schema.

use async_graphql::SchemaBuilder;
use std::time::Duration;

/// The limits of one subgraph. [`Default`] gives the values of `docs/protocol.md` §4; every setter
/// overrides one of them.
///
/// Each limit closes one way a single small request could make the server work hard:
///
/// * **body** – the smaller the body, the less the parser, the pre-check and the engine have to read.
///   Router fetches are small; parsing a worst-case body of 64 KiB took ~75 ms of synchronous work on a
///   2.1 GHz core, so the default is 16 KiB;
/// * **bracket depth** – the parser builds nested values recursively; deep brackets would overflow the
///   stack ([`super::scan_query`]). Selection sets count too, so keep it at least the nesting limit plus the
///   depth of the argument values a client needs (defaults 64 = 32 + 32);
/// * **arguments, directives, variables** – the engine compares the arguments of fields with the same
///   response name, quadratic in the number of arguments per field, and walks directives and variables per
///   location;
/// * **name length** – the engine repeats names in its error messages, once per error;
/// * **errors** – the most engine errors in one answer, and the most field-merge conflicts the pre-check
///   lets through (the engine reports each conflict once for every selection set that reaches it);
/// * **fragments, spreads, selections, nesting** – the engine expands fragments at every spread; these
///   keep every walk over the expanded document small ([`super::precheck`]);
/// * **argument values, value bytes** – the engine converts (and copies) the argument values of every field
///   instance, once per spread, a variable's value at every use; their nodes and the bytes of their texts
///   are counted through the expansion like the selections;
/// * **representations** – each `_entities` representation costs reads, and the engine's complexity
///   counts the field only once, however many representations it gets;
/// * **reads** – a hard cap at run time, because a representation can fan out into lists of any size;
/// * **concurrent reads** – bounds how many pool connections subgraph traffic takes from the app;
/// * **depth, complexity** – the engine's own limits, as a second line after the pre-check;
/// * **timeout** – a deadline for the whole execution, so a slow storage cannot hold a request (and its
///   read permits) for long. It only takes effect at await points: synchronous parsing, validation and
///   value conversion run to their end, and only the limits above bound them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubgraphLimits {
    body: usize,
    bracket_depth: usize,
    arguments: usize,
    total_arguments: usize,
    directives: usize,
    variables: usize,
    name_length: usize,
    errors: usize,
    fragments: usize,
    spreads: usize,
    selections: usize,
    values: usize,
    value_bytes: usize,
    nesting: usize,
    representations: usize,
    reads: usize,
    concurrent_reads: usize,
    depth: usize,
    complexity: usize,
    timeout: Duration,
}

impl Default for SubgraphLimits {
    fn default() -> Self {
        Self {
            body: 16 * 1024,
            bracket_depth: 64,
            arguments: 32,
            total_arguments: 1_000,
            directives: 8,
            variables: 50,
            name_length: 256,
            errors: 20,
            fragments: 30,
            spreads: 100,
            selections: 2_000,
            values: 10_000,
            value_bytes: 1024 * 1024,
            nesting: 32,
            representations: 100,
            reads: 1_000,
            concurrent_reads: 3,
            depth: 10,
            complexity: 1_000,
            timeout: Duration::from_secs(10),
        }
    }
}

impl SubgraphLimits {
    /// Largest request body in bytes (default 16 KiB); larger → `413`.
    pub fn body(mut self, bytes: usize) -> Self {
        self.body = bytes;
        self
    }

    /// Most brackets `[`, `{`, `(` open at once in the raw query text, outside strings, block strings and
    /// comments (default 64), see [`super::scan_query`].
    pub fn bracket_depth(mut self, n: usize) -> Self {
        self.bracket_depth = n;
        self
    }

    /// Most arguments written on one field or one directive (default 32).
    pub fn arguments(mut self, n: usize) -> Self {
        self.arguments = n;
        self
    }

    /// Most arguments written in one document, fields and directives together (default 1 000).
    pub fn total_arguments(mut self, n: usize) -> Self {
        self.total_arguments = n;
        self
    }

    /// Most directives on one location (operation, variable definition, field, fragment definition,
    /// spread, inline fragment; default 8). Also the engine's limit, set by [`SubgraphLimits::apply`].
    pub fn directives(mut self, n: usize) -> Self {
        self.directives = n;
        self
    }

    /// Most variable definitions of one operation (default 50).
    pub fn variables(mut self, n: usize) -> Self {
        self.variables = n;
        self
    }

    /// Longest name in bytes (default 256): operation, field, alias, argument, directive, fragment and
    /// variable names, type conditions and the types of variable definitions.
    pub fn name_length(mut self, bytes: usize) -> Self {
        self.name_length = bytes;
        self
    }

    /// Most engine errors in one answer (default 20; the rest is replaced by one note), and the most
    /// conflicts of fields with the same response name a document may have (else the pre-check rejects it).
    pub fn errors(mut self, n: usize) -> Self {
        self.errors = n;
        self
    }

    /// Most fragment definitions in one document (default 30).
    pub fn fragments(mut self, n: usize) -> Self {
        self.fragments = n;
        self
    }

    /// Most fragment spreads written in one document, not expanded (default 100).
    pub fn spreads(mut self, n: usize) -> Self {
        self.spreads = n;
        self
    }

    /// Most selections of all operations together with every spread expanded (default 2 000).
    pub fn selections(mut self, n: usize) -> Self {
        self.selections = n;
        self
    }

    /// Most value nodes in the arguments of fields and of directives on selections, of all operations
    /// together with every spread expanded (default 10 000). A variable counts the nodes of its value.
    pub fn values(mut self, n: usize) -> Self {
        self.values = n;
        self
    }

    /// Most bytes of text in the arguments of fields and of directives on selections, of all operations
    /// together with every spread expanded (default 1 MiB): strings, enum values, binary values and object
    /// keys. A variable counts the bytes of its value at every use, because the engine copies it into every
    /// field instance – enough for one record-sized variable used once, not for a large one used at hundreds
    /// of instances.
    pub fn value_bytes(mut self, bytes: usize) -> Self {
        self.value_bytes = bytes;
        self
    }

    /// Most levels of selection sets, the operation's own set being level 1 (default 32).
    pub fn nesting(mut self, n: usize) -> Self {
        self.nesting = n;
        self
    }

    /// Most `_entities` representations of one request, summed over all operations (default 100).
    pub fn representations(mut self, n: usize) -> Self {
        self.representations = n;
        self
    }

    /// Most storage reads of one request at run time (default 1 000), see [`super::ReadBudget`].
    pub fn reads(mut self, n: usize) -> Self {
        self.reads = n;
        self
    }

    /// Most reads of all subgraph requests of the process at the same time (default 3, at least 1), see
    /// [`super::ReadLimiter`].
    pub fn concurrent_reads(mut self, n: usize) -> Self {
        self.concurrent_reads = n.max(1);
        self
    }

    /// The engine's query depth limit (default 10), set by [`SubgraphLimits::apply`].
    pub fn depth(mut self, n: usize) -> Self {
        self.depth = n;
        self
    }

    /// The engine's complexity limit (default 1 000), set by [`SubgraphLimits::apply`].
    pub fn complexity(mut self, n: usize) -> Self {
        self.complexity = n;
        self
    }

    /// The deadline of one request's execution (default 10 s); when it passes, the endpoint stops the
    /// execution and answers `503 TIMEOUT`.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the engine limits on a subgraph schema: depth and complexity, the engine's recursion depth
    /// (to the nesting limit), directives per location, introspection off (`__schema` and `__type` answer `null`, `_service { sdl }`
    /// keeps working) and federation on (`_service` also without entity resolvers).
    ///
    /// Build the subgraph schema with `EmptyMutation` and `EmptySubscription`: then it cannot change
    /// anything, whatever a caller sends. Do not register extensions that rewrite the query (persisted
    /// queries, `prepare_request` hooks setting another query): the endpoint checks the document it parsed
    /// itself and hands exactly that one to the engine.
    pub fn apply<Q, M, S>(&self, builder: SchemaBuilder<Q, M, S>) -> SchemaBuilder<Q, M, S> {
        builder
            .limit_depth(self.depth)
            .limit_complexity(self.complexity)
            .limit_recursive_depth(self.nesting)
            .limit_directives(self.directives)
            .disable_introspection()
            .enable_federation()
    }
}

#[path = "limits_read.rs"]
mod read;

#[cfg(test)]
#[path = "limits_tests.rs"]
mod tests;

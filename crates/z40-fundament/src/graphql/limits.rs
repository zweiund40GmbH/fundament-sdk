//! The limits of a subgraph (`docs/protocol.md` §4): what the document pre-check, the endpoint and the
//! read budget enforce, and the engine limits [`SubgraphLimits::apply`] sets on the schema.

use async_graphql::SchemaBuilder;

/// The limits of one subgraph. [`Default`] gives the values of `docs/protocol.md` §4; every setter
/// overrides one of them.
///
/// Each limit closes one way a single small request could make the server work hard:
///
/// * **body** – the smaller the body, the less the pre-check and the engine have to read;
/// * **fragments, spreads, selections, nesting** – the engine expands fragments at every spread; these
///   keep every walk over the expanded document small ([`super::precheck`]);
/// * **representations** – each `_entities` representation costs reads, and the engine's complexity
///   counts the field only once, however many representations it gets;
/// * **reads** – a hard cap at run time, because a representation can fan out into lists of any size;
/// * **concurrent reads** – bounds how many pool connections subgraph traffic takes from the app;
/// * **depth, complexity** – the engine's own limits, as a second line after the pre-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubgraphLimits {
    body: usize,
    fragments: usize,
    spreads: usize,
    selections: usize,
    nesting: usize,
    representations: usize,
    reads: usize,
    concurrent_reads: usize,
    depth: usize,
    complexity: usize,
}

impl Default for SubgraphLimits {
    fn default() -> Self {
        Self {
            body: 64 * 1024,
            fragments: 30,
            spreads: 100,
            selections: 2_000,
            nesting: 32,
            representations: 100,
            reads: 1_000,
            concurrent_reads: 3,
            depth: 10,
            complexity: 1_000,
        }
    }
}

impl SubgraphLimits {
    /// Largest request body in bytes (default 64 KiB); larger → `413`.
    pub fn body(mut self, bytes: usize) -> Self {
        self.body = bytes;
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

    /// See [`SubgraphLimits::body`].
    pub fn max_body(&self) -> usize {
        self.body
    }

    /// See [`SubgraphLimits::fragments`].
    pub fn max_fragments(&self) -> usize {
        self.fragments
    }

    /// See [`SubgraphLimits::spreads`].
    pub fn max_spreads(&self) -> usize {
        self.spreads
    }

    /// See [`SubgraphLimits::selections`].
    pub fn max_selections(&self) -> usize {
        self.selections
    }

    /// See [`SubgraphLimits::nesting`].
    pub fn max_nesting(&self) -> usize {
        self.nesting
    }

    /// See [`SubgraphLimits::representations`].
    pub fn max_representations(&self) -> usize {
        self.representations
    }

    /// See [`SubgraphLimits::reads`].
    pub fn max_reads(&self) -> usize {
        self.reads
    }

    /// See [`SubgraphLimits::concurrent_reads`].
    pub fn max_concurrent_reads(&self) -> usize {
        self.concurrent_reads
    }

    /// See [`SubgraphLimits::depth`].
    pub fn max_depth(&self) -> usize {
        self.depth
    }

    /// See [`SubgraphLimits::complexity`].
    pub fn max_complexity(&self) -> usize {
        self.complexity
    }

    /// Sets the engine limits on a subgraph schema: depth and complexity, the engine's recursion depth
    /// (to the nesting limit), introspection off (`__schema` and `__type` answer `null`, `_service { sdl }`
    /// keeps working) and federation on (`_service` also without entity resolvers).
    ///
    /// Build the subgraph schema with `EmptyMutation` and `EmptySubscription`: then it cannot change
    /// anything, whatever a caller sends.
    pub fn apply<Q, M, S>(&self, builder: SchemaBuilder<Q, M, S>) -> SchemaBuilder<Q, M, S> {
        builder
            .limit_depth(self.depth)
            .limit_complexity(self.complexity)
            .limit_recursive_depth(self.nesting)
            .disable_introspection()
            .enable_federation()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_the_protocol() {
        let l = SubgraphLimits::default();
        assert_eq!(l.max_body(), 65_536);
        assert_eq!(l.max_fragments(), 30);
        assert_eq!(l.max_spreads(), 100);
        assert_eq!(l.max_selections(), 2_000);
        assert_eq!(l.max_nesting(), 32);
        assert_eq!(l.max_representations(), 100);
        assert_eq!(l.max_reads(), 1_000);
        assert_eq!(l.max_concurrent_reads(), 3);
        assert_eq!(l.max_depth(), 10);
        assert_eq!(l.max_complexity(), 1_000);
    }

    #[test]
    fn setters_override_one_value() {
        let l = SubgraphLimits::default()
            .representations(5)
            .concurrent_reads(0);
        assert_eq!(l.max_representations(), 5);
        assert_eq!(l.max_concurrent_reads(), 1, "0 would block every read");
        assert_eq!(l.max_reads(), 1_000);
    }
}

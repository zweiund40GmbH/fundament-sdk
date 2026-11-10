//! The read side of [`SubgraphLimits`]: one getter per limit, documented at its setter.

use super::SubgraphLimits;
use std::time::Duration;

impl SubgraphLimits {
    /// See [`SubgraphLimits::body`].
    pub fn max_body(&self) -> usize {
        self.body
    }

    /// See [`SubgraphLimits::bracket_depth`].
    pub fn max_bracket_depth(&self) -> usize {
        self.bracket_depth
    }

    /// See [`SubgraphLimits::arguments`].
    pub fn max_arguments(&self) -> usize {
        self.arguments
    }

    /// See [`SubgraphLimits::total_arguments`].
    pub fn max_total_arguments(&self) -> usize {
        self.total_arguments
    }

    /// See [`SubgraphLimits::directives`].
    pub fn max_directives(&self) -> usize {
        self.directives
    }

    /// See [`SubgraphLimits::variables`].
    pub fn max_variables(&self) -> usize {
        self.variables
    }

    /// See [`SubgraphLimits::name_length`].
    pub fn max_name_length(&self) -> usize {
        self.name_length
    }

    /// See [`SubgraphLimits::errors`].
    pub fn max_errors(&self) -> usize {
        self.errors
    }

    /// See [`SubgraphLimits::timeout`].
    pub fn max_duration(&self) -> Duration {
        self.timeout
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

    /// See [`SubgraphLimits::values`].
    pub fn max_values(&self) -> usize {
        self.values
    }

    /// See [`SubgraphLimits::value_bytes`].
    pub fn max_value_bytes(&self) -> usize {
        self.value_bytes
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
}

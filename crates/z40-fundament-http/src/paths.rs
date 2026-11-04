//! Path rules shared by [`crate::no_store::NoStore`] and [`crate::csrf::Csrf`]: a set of exact paths and
//! path prefixes.
//!
//! Matching uses the raw request path (`Uri::path`, without query and without percent-decoding), like
//! axum's router. In a nested router axum strips the nesting prefix, so add these layers to the outer
//! router when the rules name full paths.

use std::sync::Arc;

/// Exact paths and prefixes. Empty = matches nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PathSet {
    exact: Vec<Arc<str>>,
    prefixes: Vec<Arc<str>>,
}

impl PathSet {
    /// Adds the exact path `path`.
    pub(crate) fn add_path(&mut self, path: &str) {
        self.exact.push(path.into());
    }

    /// Adds the prefix `prefix` (e.g. `/api/` matches `/api/x`, not `/apix`).
    pub(crate) fn add_prefix(&mut self, prefix: &str) {
        self.prefixes.push(prefix.into());
    }

    /// Whether no rule was added.
    pub(crate) fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.prefixes.is_empty()
    }

    /// Whether `path` is one of the exact paths or starts with one of the prefixes.
    pub(crate) fn matches(&self, path: &str) -> bool {
        self.exact.iter().any(|p| **p == *path)
            || self.prefixes.iter().any(|p| path.starts_with(&**p))
    }
}

/// Which paths a layer applies to: all (when no rule was added) or the added ones, minus exceptions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Scope {
    /// Paths the layer applies to; empty = all.
    pub(crate) only: PathSet,
    /// Paths the layer never applies to.
    pub(crate) except: PathSet,
}

impl Scope {
    /// Whether the layer applies to `path`.
    pub(crate) fn applies(&self, path: &str) -> bool {
        (self.only.is_empty() || self.only.matches(path)) && !self.except.matches(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_and_prefix() {
        let mut s = PathSet::default();
        assert!(s.is_empty() && !s.matches("/api/x"));
        s.add_prefix("/api/");
        s.add_path("/auth/logout");
        assert!(s.matches("/api/x") && s.matches("/auth/logout"));
        assert!(!s.matches("/apix") && !s.matches("/auth/logout/x") && !s.matches("/api"));
    }

    #[test]
    fn scope_defaults_to_all_and_honours_exceptions() {
        let mut scope = Scope::default();
        assert!(scope.applies("/anything"));
        scope.only.add_prefix("/api/");
        scope.except.add_path("/api/info");
        assert!(scope.applies("/api/me"));
        assert!(!scope.applies("/api/info") && !scope.applies("/healthz"));
    }
}

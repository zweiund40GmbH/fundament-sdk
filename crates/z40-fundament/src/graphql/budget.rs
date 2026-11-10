//! The read limits of a subgraph at run time (`docs/protocol.md` §4.4). The pre-check cannot know how
//! many rows a representation fans out into, so every storage read of a resolver goes through
//! [`ReadBudget::read`]:
//!
//! * it counts the read and refuses (`VALIDATION`) once the request has made its budget of reads, without
//!   running the refused read;
//! * it holds one permit of the process-wide [`ReadLimiter`] while the read runs, so subgraph traffic
//!   takes at most that many pool connections. The permit is held only around that one read, never while
//!   waiting for another permit – so do not call [`ReadBudget::read`] inside a read.

use super::SubgraphLimits;
use crate::http::MSG_INTERNAL;
use async_graphql::{Context, ErrorExtensions};
use std::fmt::Display;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Semaphore;

/// Message of a read beyond the budget (`docs/protocol.md` §4.4).
const MSG_TOO_MANY_READS: &str = "Zu viele Lesezugriffe in einer Anfrage.";

/// The process-wide limit of concurrent subgraph reads: a semaphore. Cheap to clone; all clones share
/// the permits. Create one per process (the [`super::SubgraphEndpoint`] does) and pass it to every
/// [`ReadBudget`].
#[derive(Debug, Clone)]
pub struct ReadLimiter(Arc<Semaphore>);

impl ReadLimiter {
    /// A limiter of `permits` concurrent reads (at least 1).
    pub fn new(permits: usize) -> Self {
        Self(Arc::new(Semaphore::new(permits.max(1))))
    }

    /// A limiter of [`SubgraphLimits::max_concurrent_reads`] permits.
    pub fn from_limits(limits: &SubgraphLimits) -> Self {
        Self::new(limits.max_concurrent_reads())
    }

    /// Permits free right now (for tests and metrics).
    pub fn available(&self) -> usize {
        self.0.available_permits()
    }
}

impl Default for ReadLimiter {
    /// A limiter with the default of [`SubgraphLimits`] (3).
    fn default() -> Self {
        Self::from_limits(&SubgraphLimits::default())
    }
}

/// The read budget of one subgraph request. Cheap to clone; clones share the count. The endpoint puts it
/// into the request data, resolvers get it with [`ReadBudget::from_ctx`].
#[derive(Debug, Clone)]
pub struct ReadBudget(Arc<Inner>);

/// Shared state of a [`ReadBudget`].
#[derive(Debug)]
struct Inner {
    reads: AtomicUsize,
    cap: usize,
    limiter: ReadLimiter,
}

impl ReadBudget {
    /// A fresh budget of `cap` reads, sharing the permits of `limiter`.
    pub fn new(cap: usize, limiter: ReadLimiter) -> Self {
        Self(Arc::new(Inner {
            reads: AtomicUsize::new(0),
            cap,
            limiter,
        }))
    }

    /// A fresh budget of [`SubgraphLimits::max_reads`] reads.
    pub fn from_limits(limits: &SubgraphLimits, limiter: ReadLimiter) -> Self {
        Self::new(limits.max_reads(), limiter)
    }

    /// The budget of the current request (request data set by [`super::SubgraphEndpoint`]). Missing data
    /// is a setup error of the app: logged, and answered with a generic `INTERNAL` error.
    pub fn from_ctx<'a>(ctx: &Context<'a>) -> async_graphql::Result<&'a ReadBudget> {
        ctx.data_opt::<ReadBudget>().ok_or_else(|| {
            tracing::error!("subgraph resolver without ReadBudget in the request data");
            error("INTERNAL", MSG_INTERNAL)
        })
    }

    /// Reads started so far (never more than [`ReadBudget::cap`]), e.g. for the log line of a request.
    pub fn reads(&self) -> usize {
        self.0.reads.load(Ordering::Relaxed)
    }

    /// The most reads this budget allows.
    pub fn cap(&self) -> usize {
        self.0.cap
    }

    /// Runs the storage read `read` if the budget allows another one – else an error with
    /// `extensions.code = "VALIDATION"`, and `read` is never polled – holding one permit of the limiter
    /// while it runs. The permit is released when `read` finishes, fails or is dropped.
    pub async fn read<F: Future>(&self, read: F) -> async_graphql::Result<F::Output> {
        let cap = self.0.cap;
        let counted = self
            .0
            .reads
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < cap).then_some(n + 1)
            });
        if counted.is_err() {
            return Err(error("VALIDATION", MSG_TOO_MANY_READS));
        }
        // The semaphore is never closed; an error would be a bug of this crate, not of the client.
        let _permit = self.0.limiter.0.acquire().await.map_err(|_| {
            tracing::error!("subgraph read limiter closed");
            error("INTERNAL", MSG_INTERNAL)
        })?;
        Ok(read.await)
    }

    /// [`ReadBudget::read`] for a storage read that can fail: a failure is logged and answered with the
    /// generic [`internal`] error, so the text of a storage error never reaches the client.
    pub async fn read_or_internal<F, T, E>(&self, read: F) -> async_graphql::Result<T>
    where
        F: Future<Output = Result<T, E>>,
        E: Display,
    {
        self.read(read).await?.map_err(internal)
    }
}

/// The error of a resolver whose storage (or any other internal step) failed: logs `err` and returns
/// a generic error with `extensions.code = "INTERNAL"` and the fixed text of `docs/protocol.md` §4.5.
///
/// Use it instead of `?` on storage errors: async-graphql turns every `Display` error into an error whose
/// message is that text, which may name tables, columns or hosts.
pub fn internal(err: impl Display) -> async_graphql::Error {
    tracing::error!(error = %err, "subgraph resolver failed");
    error("INTERNAL", MSG_INTERNAL)
}

/// An async-graphql error with `extensions.code`.
fn error(code: &'static str, message: &str) -> async_graphql::Error {
    async_graphql::Error::new(message).extend_with(|_, e| e.set("code", code))
}

#[cfg(test)]
#[path = "budget_tests.rs"]
mod tests;

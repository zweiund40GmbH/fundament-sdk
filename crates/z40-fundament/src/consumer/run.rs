//! [`FeedConsumer`]: the builder and one run. The page logic (parsing, cursor rules) is pure and lives
//! in [`crate::protocol::feed`]; this module only sequences IO.

use super::client::{FeedClient, FetchError};
use super::config::FeedConsumerConfig;
use super::{CursorStore, PageSink, RunLock, StoreError};
use crate::credentials::{BearerKey, name_is_valid};
use crate::http_client::{https_origin, loopback_http_origin};
use crate::protocol::MAX_PAGE_LIMIT;
use crate::protocol::feed::parse_page;
use chrono::Utc;

/// Default pages per run.
pub const DEFAULT_MAX_PAGES: usize = 20;

/// Why a consumer could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BuildError {
    /// The source name is not 1–64 characters of `a-z A-Z 0-9 _ . -`.
    #[error("invalid source name")]
    InvalidSource,
    /// The base URL is not an `https://` origin (or, for tests, a loopback `http://` origin).
    #[error("invalid source origin")]
    InvalidUrl,
    /// The page limit is not within 1..=500.
    #[error("page limit must be within 1..=500")]
    InvalidPageLimit,
    /// Pages per run must be at least 1.
    #[error("max pages per run must be at least 1")]
    InvalidMaxPages,
    /// The HTTP client could not be created.
    #[error("HTTP client could not be created")]
    Client,
}

/// Builder of a [`FeedConsumer`].
#[derive(Debug, Clone)]
pub struct FeedConsumerBuilder {
    source: String,
    origin: String,
    https_only: bool,
    key: BearerKey,
    types: Vec<String>,
    page_limit: u32,
    max_pages: usize,
}

impl FeedConsumerBuilder {
    /// A consumer of the feed at the `https://` origin `base_url`, stored under the name `source`.
    pub fn new(source: &str, base_url: &str, key: BearerKey) -> Result<Self, BuildError> {
        let origin = https_origin(base_url).ok_or(BuildError::InvalidUrl)?;
        Self::with_origin(source, origin, key, true)
    }

    /// Like [`FeedConsumerBuilder::new`], but for a plain `http://` origin on a **loopback** address
    /// (a fake feed server in tests). Configuration never leads here.
    pub fn plain_http_for_tests(
        source: &str,
        base_url: &str,
        key: BearerKey,
    ) -> Result<Self, BuildError> {
        let origin = loopback_http_origin(base_url).ok_or(BuildError::InvalidUrl)?;
        Self::with_origin(source, origin, key, false)
    }

    fn with_origin(
        source: &str,
        origin: String,
        key: BearerKey,
        https_only: bool,
    ) -> Result<Self, BuildError> {
        if !name_is_valid(source) {
            return Err(BuildError::InvalidSource);
        }
        Ok(Self {
            source: source.to_owned(),
            origin,
            https_only,
            key,
            types: Vec::new(),
            page_limit: MAX_PAGE_LIMIT,
            max_pages: DEFAULT_MAX_PAGES,
        })
    }

    /// Requests only these record types (default: every type the source allows).
    pub fn types<I, T>(mut self, types: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        self.types = types.into_iter().map(Into::into).collect();
        self
    }

    /// Events per page, 1..=500 (default 500).
    pub fn page_limit(mut self, limit: u32) -> Self {
        self.page_limit = limit;
        self
    }

    /// Pages per run at most (default [`DEFAULT_MAX_PAGES`]); the next run continues.
    pub fn max_pages(mut self, pages: usize) -> Self {
        self.max_pages = pages;
        self
    }

    /// The consumer.
    pub fn build(self) -> Result<FeedConsumer, BuildError> {
        if !(1..=MAX_PAGE_LIMIT).contains(&self.page_limit) {
            return Err(BuildError::InvalidPageLimit);
        }
        if self.max_pages == 0 {
            return Err(BuildError::InvalidMaxPages);
        }
        let client = FeedClient::new(&self.origin, self.key, self.https_only)
            .map_err(|_| BuildError::Client)?;
        Ok(FeedConsumer {
            source: self.source,
            client,
            types: self.types.join(","),
            page_limit: self.page_limit,
            max_pages: self.max_pages,
        })
    }
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// Another run held the lock; nothing happened.
    Locked,
    /// The run succeeded.
    Ok,
    /// The source failed; pages applied before the failure stay applied.
    Failed(FetchError),
}

/// Result of [`FeedConsumer::run_once`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunReport {
    /// How the run ended.
    pub outcome: RunOutcome,
    /// Pages requested.
    pub pages: usize,
    /// Records written.
    pub written: usize,
    /// Cursor after the run (`0` when the run was locked out).
    pub cursor: u64,
}

/// Reads one feed. Build with [`FeedConsumer::builder`] or [`FeedConsumer::from_config`].
#[derive(Debug)]
pub struct FeedConsumer {
    source: String,
    client: FeedClient,
    types: String,
    page_limit: u32,
    max_pages: usize,
}

/// Why fetching stopped.
enum Stop {
    Fetch(FetchError),
    Store(StoreError),
}

impl FeedConsumer {
    /// Builder for the `https://` origin `base_url` (see [`FeedConsumerBuilder::new`]).
    pub fn builder(
        source: &str,
        base_url: &str,
        key: BearerKey,
    ) -> Result<FeedConsumerBuilder, BuildError> {
        FeedConsumerBuilder::new(source, base_url, key)
    }

    /// Builder from the configuration; `Ok(None)` when the feed is off (URL or key unset).
    pub fn from_config(
        source: &str,
        config: &FeedConsumerConfig,
    ) -> Result<Option<FeedConsumerBuilder>, BuildError> {
        match (&config.url, &config.key) {
            (Some(url), Some(key)) => FeedConsumerBuilder::new(source, url, key.clone()).map(Some),
            _ => Ok(None),
        }
    }

    /// Name of the source (key of the cursor).
    pub fn source(&self) -> &str {
        &self.source
    }

    /// One run under the lock of the source: pages from the stored cursor until `hasMore` is `false`
    /// or the page budget is used, each page applied with its cursor; on `gap` the cursor goes back to
    /// 0 and reading continues. A failure of the source is recorded and reported in
    /// [`RunReport::outcome`]; only a failure of the store is an `Err`.
    pub async fn run_once<S>(&self, store: &S) -> Result<RunReport, StoreError>
    where
        S: RunLock + CursorStore + PageSink,
    {
        let Some(guard) = store.try_lock(&self.source).await? else {
            return Ok(RunReport {
                outcome: RunOutcome::Locked,
                pages: 0,
                written: 0,
                cursor: 0,
            });
        };
        let mut report = RunReport {
            outcome: RunOutcome::Ok,
            pages: 0,
            written: 0,
            cursor: 0,
        };
        let result = match self.fetch(store, &mut report).await {
            Ok(()) => store.finish_run(&self.source, None, Utc::now()).await,
            Err(Stop::Fetch(e)) => {
                report.outcome = RunOutcome::Failed(e);
                tracing::warn!(source = %self.source, error = %e, pages = report.pages, "feed run failed");
                store.finish_run(&self.source, Some(e), Utc::now()).await
            }
            Err(Stop::Store(e)) => Err(e),
        };
        if let Err(e) = store.unlock(guard).await {
            tracing::warn!(source = %self.source, error = %e, "releasing the feed lock failed");
        }
        result.map(|()| report)
    }

    /// Fetches and applies pages, updating `report` as it goes.
    async fn fetch<S>(&self, store: &S, report: &mut RunReport) -> Result<(), Stop>
    where
        S: CursorStore + PageSink,
    {
        let start = store.load(&self.source).await.map_err(Stop::Store)?;
        report.cursor = start.map_or(0, |c| c.cursor);
        while report.pages < self.max_pages {
            report.pages += 1;
            let body = self
                .client
                .page(report.cursor, self.page_limit, &self.types)
                .await
                .map_err(Stop::Fetch)?;
            let page =
                parse_page(&body, report.cursor).map_err(|_| Stop::Fetch(FetchError::Invalid))?;
            if page.gap && report.cursor != 0 {
                // Events after the cursor are gone: read everything again from 0.
                store.reset(&self.source).await.map_err(Stop::Store)?;
                report.cursor = 0;
                continue;
            }
            report.written += store
                .apply_page(&self.source, &page.items, page.next_cursor)
                .await
                .map_err(Stop::Store)?;
            report.cursor = page.next_cursor;
            if !page.has_more {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;

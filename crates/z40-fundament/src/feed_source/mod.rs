//! Serving a change feed (`GET /fundament/v1/events`, `docs/protocol.md` §2): the producer side of the
//! protocol. The app writes an event into its outbox **in the transaction of each change**; afterwards a
//! cursor assigner numbers the committed events, and [`FeedEndpoint`] serves them page by page.
//!
//! The app connects its storage through [`FeedSourceStore`] (and [`SourceStatusStore`] for the status
//! document); `feed-source-mysql` ships a MariaDB/MySQL implementation of both.
//!
//! * [`FeedQuery`] – the rules of `since`, `limit` and `types`
//! * [`build_page`] – the page from what the store read (`hasMore` via `limit + 1`)
//! * [`FeedEndpoint`] – the axum route: brake → feed key → query → cursor assignment → read
//! * [`assign_cursors_once`] – one round of the cursor assigner, for the app's worker
//! * [`FeedSourceConfig`] – feed keys and retention from `from_lookup`
//!
//! **Why cursors are assigned after commit.** A transaction that wrote its event first may commit after
//! a later one. Numbering at write time would let a consumer move past the earlier cursor before it
//! becomes visible, and it would miss the event for good. The assigner numbers only committed events and
//! never skips one: an event of a still open transaction gets a higher cursor later. Run
//! [`assign_cursors_once`] every [`ASSIGN_INTERVAL`] in the worker; the endpoint additionally runs it
//! before every answer.

mod config;
mod endpoint;
mod failures;
mod page;
mod query;
#[cfg(feature = "status")]
mod status;

pub use config::{DEFAULT_RETENTION_DAYS, FeedSourceConfig, RETENTION_DAYS_RANGE};
pub use endpoint::FeedEndpoint;
pub use page::build_page;
pub use query::{DEFAULT_LIMIT, FeedQuery, QueryError, TYPE_MAX, is_valid_type};
#[cfg(feature = "status")]
pub use status::{failed_attempts_note, offers, poller, source_status};

use crate::StoreError;
use crate::protocol::feed::FeedEvent;
use chrono::{DateTime, Utc};
use std::future::Future;
use std::time::Duration;

/// How often the app's worker should run [`assign_cursors_once`].
pub const ASSIGN_INTERVAL: Duration = Duration::from_secs(2);
/// Longest wait of the worker's assigner for an event of an open transaction.
pub const WORKER_ASSIGN_WAIT: Duration = Duration::from_secs(5);
/// Longest wait of the assigner a feed request runs before answering: short, so a long transaction does
/// not hold up the answer (the worker assigns the waiting events later).
pub const REQUEST_ASSIGN_WAIT: Duration = Duration::from_secs(1);

/// Result of one round of the cursor assigner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOutcome {
    /// `n` events got a cursor; none is left waiting.
    Assigned(u64),
    /// `n` events got a cursor, then an open transaction held further events longer than the store
    /// waits. They get a (higher) cursor in a later round; none is skipped.
    Waiting(u64),
    /// Another instance was assigning at the same time; what it assigns is visible to everyone.
    Skipped,
}

/// The storage of a feed source, implemented by the app (or by `feed-source-mysql`).
pub trait FeedSourceStore: Send + Sync + 'static {
    /// The record type URNs this source offers (each valid by [`is_valid_type`]). A request without
    /// `types` reads all of them; any other type in `types` is answered with `400`.
    fn known_types(&self) -> &[String];

    /// Gives every committed event without a cursor the next cursors, in the order of origin, and
    /// never reuses or skips one (see the module docs). Waits at most `max_wait` for an event of a still
    /// open transaction. Must serialise concurrent assigners (e.g. a database lock; a held lock means
    /// [`AssignOutcome::Skipped`], not waiting) and make the new cursors visible all at once.
    fn assign_cursors(
        &self,
        max_wait: Duration,
    ) -> impl Future<Output = Result<AssignOutcome, StoreError>> + Send;

    /// The last cursor ever assigned, when the store knows it: every event up to it is committed and
    /// readable. Lets a consumer of rare types skip events of other types ([`build_page`]). The default
    /// `None` disables that.
    fn last_cursor(&self) -> impl Future<Output = Result<Option<u64>, StoreError>> + Send {
        async { Ok(None) }
    }

    /// Up to `limit` events of `types` with a cursor above `since`, in ascending cursor order. The
    /// endpoint asks for one more than the page size to learn `hasMore`. `data` must hold only the
    /// fields consumers may see.
    fn read(
        &self,
        since: u64,
        limit: u32,
        types: &[String],
    ) -> impl Future<Output = Result<Vec<FeedEvent>, StoreError>> + Send;

    /// Remembers that the consumer `consumer` (the name of its feed key) polled at `at` and was answered
    /// with `next_cursor` – the `abrufer` of the status document.
    fn record_poll(
        &self,
        consumer: &str,
        next_cursor: u64,
        at: DateTime<Utc>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Adds `count` requests with a missing or wrong feed key at `at` (no address, no key). The
    /// endpoint counts in memory and calls this from at most one task at a time. The total of 24 hours
    /// appears as a note in the status document.
    fn record_failures(
        &self,
        count: u64,
        at: DateTime<Utc>,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;
}

/// Figures of one record type for `angebote` of the status document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OfferFigures {
    /// Record type URN.
    pub record_type: String,
    /// Newest cursor of the type (`0` = none yet).
    pub last_cursor: u64,
    /// Events of the last 5 minutes.
    pub last_5_minutes: u64,
    /// Events of the last 24 hours.
    pub last_24_hours: u64,
}

/// The last poll of one consumer, as remembered by [`FeedSourceStore::record_poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollRecord {
    /// Name of the feed key.
    pub consumer: String,
    /// When it polled last.
    pub last_poll: DateTime<Utc>,
    /// `nextCursor` of that answer.
    pub cursor: u64,
}

/// What the status document of a source needs from its storage (`angebote`, `abrufer`, failed
/// attempts). Every query must be bounded, so the status answers within 2 seconds.
pub trait SourceStatusStore: Send + Sync {
    /// Figures per record type at `now` (types without events may be missing). A store may measure the
    /// windows against its own clock, the one its events are stamped with.
    fn offer_figures(
        &self,
        now: DateTime<Utc>,
    ) -> impl Future<Output = Result<Vec<OfferFigures>, StoreError>> + Send;

    /// The remembered polls, one per consumer name ever seen.
    fn polls(&self) -> impl Future<Output = Result<Vec<PollRecord>, StoreError>> + Send;

    /// Events with a cursor above `cursor`, counted up to `cap`.
    fn backlog(
        &self,
        cursor: u64,
        cap: u64,
    ) -> impl Future<Output = Result<u64, StoreError>> + Send;

    /// Failed feed key attempts since `since` (all addresses together).
    fn failed_attempts(
        &self,
        since: DateTime<Utc>,
    ) -> impl Future<Output = Result<u64, StoreError>> + Send;
}

/// One round of the cursor assigner with logging, waiting at most [`WORKER_ASSIGN_WAIT`] for open
/// transactions. The app's worker calls this every [`ASSIGN_INTERVAL`]; the endpoint runs a round with
/// [`REQUEST_ASSIGN_WAIT`] before each answer.
///
/// A long transaction that writes outbox rows delays the feed by at most its own duration (later events
/// wait for it, to keep the order of origin): keep such transactions short.
pub async fn assign_cursors_once<S: FeedSourceStore>(
    store: &S,
) -> Result<AssignOutcome, StoreError> {
    assign_logged(store, WORKER_ASSIGN_WAIT).await
}

/// One round with `max_wait`, logged.
pub(crate) async fn assign_logged<S: FeedSourceStore>(
    store: &S,
    max_wait: Duration,
) -> Result<AssignOutcome, StoreError> {
    let outcome = store.assign_cursors(max_wait).await;
    match &outcome {
        Ok(AssignOutcome::Assigned(n)) if *n > 0 => {
            tracing::debug!(count = n, "feed cursors assigned");
        }
        Ok(AssignOutcome::Waiting(n)) => {
            tracing::info!(
                count = n,
                "feed cursor assignment waits for an open transaction"
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "feed cursor assignment failed"),
    }
    outcome
}

//! Counting failed feed key attempts without letting guessers create database work: each `401` only
//! adds to an in-memory counter; at most **one** task at a time writes the pending count to the store
//! ([`FeedSourceStore::record_failures`]). Many failures while a write is running are added up and
//! written by the next round of the same task.

use super::FeedSourceStore;
use chrono::Utc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Pending failed attempts and whether a writer task runs.
#[derive(Debug, Default)]
pub(crate) struct FailureCounter {
    pending: AtomicU64,
    writing: AtomicBool,
}

impl FailureCounter {
    /// Counts one failed attempt and starts the writer task if none runs.
    pub fn count<S: FeedSourceStore>(self: &Arc<Self>, store: &Arc<S>) {
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.try_start() {
            let (me, store) = (Arc::clone(self), Arc::clone(store));
            tokio::spawn(async move { me.write(&*store).await });
        }
    }

    /// Takes the writer role; `false` when a writer runs.
    fn try_start(&self) -> bool {
        self.writing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Writes pending counts until none is left. A failed write is logged and its count dropped (the
    /// figure is advisory; retrying could pile up).
    async fn write<S: FeedSourceStore>(&self, store: &S) {
        loop {
            let n = self.pending.swap(0, Ordering::SeqCst);
            if n > 0
                && let Err(e) = store.record_failures(n, Utc::now()).await
            {
                tracing::warn!(error = %e, count = n, "failed feed attempts could not be counted");
            }
            self.writing.store(false, Ordering::SeqCst);
            // A failure counted after the swap but before the release found the role taken: take it
            // back and write it, unless another task did.
            if self.pending.load(Ordering::SeqCst) == 0 || !self.try_start() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoreError;
    use crate::feed_source::AssignOutcome;
    use crate::protocol::feed::FeedEvent;
    use chrono::DateTime;
    use std::time::Duration;
    use tokio::sync::Semaphore;

    /// A store whose `record_failures` blocks until the test opens the gate.
    struct Blocking {
        gate: Semaphore,
        in_flight: AtomicU64,
        max_in_flight: AtomicU64,
        calls: AtomicU64,
        total: AtomicU64,
    }

    impl Blocking {
        fn new() -> Self {
            Self {
                gate: Semaphore::new(0),
                in_flight: AtomicU64::new(0),
                max_in_flight: AtomicU64::new(0),
                calls: AtomicU64::new(0),
                total: AtomicU64::new(0),
            }
        }
    }

    impl FeedSourceStore for Blocking {
        fn known_types(&self) -> &[String] {
            &[]
        }
        async fn assign_cursors(&self, _: Duration) -> Result<AssignOutcome, StoreError> {
            Ok(AssignOutcome::Skipped)
        }
        async fn read(&self, _: u64, _: u32, _: &[String]) -> Result<Vec<FeedEvent>, StoreError> {
            Ok(Vec::new())
        }
        async fn record_poll(&self, _: &str, _: u64, _: DateTime<Utc>) -> Result<(), StoreError> {
            Ok(())
        }
        async fn record_failures(&self, n: u64, _: DateTime<Utc>) -> Result<(), StoreError> {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.gate.acquire().await.unwrap().forget();
            self.total.fetch_add(n, Ordering::SeqCst);
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn one_writer_and_nothing_lost() {
        let store = Arc::new(Blocking::new());
        let counter = Arc::new(FailureCounter::default());
        for _ in 0..1000 {
            counter.count(&store);
            tokio::task::yield_now().await;
        }
        assert_eq!(
            store.calls.load(Ordering::SeqCst),
            1,
            "one blocked write, the rest pending"
        );
        store.gate.add_permits(1000);
        for _ in 0..100 {
            if store.total.load(Ordering::SeqCst) == 1000 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            store.total.load(Ordering::SeqCst),
            1000,
            "every failure counted"
        );
        assert_eq!(store.max_in_flight.load(Ordering::SeqCst), 1);
        assert!(
            store.calls.load(Ordering::SeqCst) <= 3,
            "pending counts written together"
        );
    }
}

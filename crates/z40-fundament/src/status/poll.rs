//! Polling the status of many apps: [`StatusPoller`] holds one HTTP client (one connection pool, the
//! trusted roots loaded once) for any number of targets; each request carries only the key of its
//! target. [`poll_all`] fetches many targets with bounded concurrency. Scheduling (how often to poll)
//! stays in the app.

use super::client::{StatusClientError, StatusFetchError, StatusTarget, fetch_document, http};
use super::document::StatusDocument;
use super::keys::StatusKeys;
use crate::credentials::BearerKey;
use crate::protocol::STATUS_PATH;
use tokio::task::JoinSet;

/// The outcome of one target: its app name and the document or the error.
pub type PollResult = (String, Result<StatusDocument, StatusFetchError>);

/// One HTTP client for the status of many apps. Build it once and keep it (e.g. in the app state):
/// connections and the loaded trusted roots are reused across fetches. Cheap to clone. The rules of
/// [`super::StatusClient`] apply to every fetch (deadline, no redirects, body limit, checked document).
#[derive(Debug, Clone)]
pub struct StatusPoller {
    http: reqwest::Client,
}

impl StatusPoller {
    /// A poller for `https://` targets ([`StatusTarget::new`]).
    pub fn new() -> Result<Self, StatusClientError> {
        Ok(Self { http: http(true)? })
    }

    /// A poller that also accepts plain `http://` targets; only loopback targets can be built for it
    /// ([`StatusTarget::plain_http_for_tests`]). For tests against a local server; only with the
    /// test-only feature `test-util`.
    #[cfg(any(test, feature = "test-util"))]
    #[cfg_attr(docsrs, doc(cfg(feature = "test-util")))]
    pub fn plain_http_for_tests() -> Result<Self, StatusClientError> {
        Ok(Self { http: http(false)? })
    }

    /// Fetches and checks the status document of `target`, sending `key`.
    pub async fn fetch(
        &self,
        target: &StatusTarget,
        key: &BearerKey,
    ) -> Result<StatusDocument, StatusFetchError> {
        let url = format!("{}{STATUS_PATH}", target.origin());
        fetch_document(&self.http, &url, key).await
    }

    /// Fetches the status of every target that has a key in `keys` (targets without a key are skipped
    /// and do not appear in the result), at most `concurrency` at a time (`0` counts as `1`). The
    /// results are in the order of `targets`; each fetch has its own [`super::FETCH_DEADLINE`], so a
    /// call takes about `ceil(polled / concurrency) × 2 s` at worst. Needs a Tokio runtime (the
    /// fetches run as tasks).
    pub async fn poll_all(
        &self,
        targets: &[StatusTarget],
        keys: &StatusKeys,
        concurrency: usize,
    ) -> Vec<PollResult> {
        let polled: Vec<(&StatusTarget, &BearerKey)> = targets
            .iter()
            .filter_map(|t| Some((t, keys.get(t.app())?)))
            .collect();
        let mut results: Vec<Option<Result<StatusDocument, StatusFetchError>>> =
            (0..polled.len()).map(|_| None).collect();
        let mut running = JoinSet::new();
        for (i, (target, key)) in polled.iter().enumerate() {
            if running.len() >= concurrency.max(1) {
                collect_one(&mut running, &mut results).await;
            }
            let (me, target, key) = (self.clone(), (*target).clone(), (*key).clone());
            running.spawn(async move { (i, me.fetch(&target, &key).await) });
        }
        while !running.is_empty() {
            collect_one(&mut running, &mut results).await;
        }
        polled
            .into_iter()
            .zip(results)
            // Every slot is filled: each task either returns or its panic is resumed below.
            .map(|((t, _), r)| {
                let r = r.unwrap_or(Err(StatusFetchError::Unreachable));
                (t.app().to_owned(), r)
            })
            .collect()
    }
}

/// Fetches the status of every target that has a key in `keys`: [`StatusPoller::poll_all`] on a new
/// poller. A poller calling this every cycle loads the trusted roots and opens connections each time;
/// keep a [`StatusPoller`] instead.
pub async fn poll_all(
    targets: &[StatusTarget],
    keys: &StatusKeys,
    concurrency: usize,
) -> Result<Vec<PollResult>, StatusClientError> {
    Ok(StatusPoller::new()?
        .poll_all(targets, keys, concurrency)
        .await)
}

/// Waits for one finished fetch and stores its result; a panic of the task is resumed here.
async fn collect_one(
    running: &mut JoinSet<(usize, Result<StatusDocument, StatusFetchError>)>,
    results: &mut [Option<Result<StatusDocument, StatusFetchError>>],
) {
    match running.join_next().await {
        Some(Ok((i, r))) => {
            if let Some(slot) = results.get_mut(i) {
                *slot = Some(r);
            }
        }
        Some(Err(e)) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        // Cancelled tasks cannot occur (nothing aborts them); an empty set returns `None`.
        _ => {}
    }
}

// The tests serve the library's own endpoint, which needs the `status` feature.
#[cfg(test)]
#[cfg(feature = "status")]
mod tests {
    use super::*;
    use crate::credentials::{KeyList, key_entry};
    use crate::status::{StatusBuilder, StatusEndpoint, StatusProvider};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// Answers after 100 ms and records the highest number of parallel requests.
    #[derive(Clone)]
    struct Slow {
        app: &'static str,
        now: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    impl StatusProvider for Slow {
        async fn status(
            &self,
            _caller: &str,
        ) -> Result<StatusDocument, crate::status::handler::BoxError> {
            let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(100)).await;
            self.now.fetch_sub(1, Ordering::SeqCst);
            Ok(StatusBuilder::new(self.app, "1.0.0", chrono::Utc::now()).build())
        }
    }

    /// Serves `provider` on 127.0.0.1 with the status key `key`; returns the base URL.
    async fn serve(provider: Slow, key: &str) -> String {
        let keys = KeyList::parse(&key_entry("monitor", key).unwrap()).unwrap();
        let app: axum::Router = StatusEndpoint::new(provider, keys).router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let service = app.into_make_service_with_connect_info::<std::net::SocketAddr>();
        tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn bounded_parallel_polls_in_target_order() {
        let (now, peak) = (Arc::default(), Arc::new(AtomicUsize::new(0)));
        let mut targets = Vec::new();
        let mut raw_keys = Vec::new();
        for (i, app) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
            let provider = Slow {
                app,
                now: Arc::clone(&now),
                peak: Arc::clone(&peak),
            };
            let base = serve(provider, &format!("key-{i}")).await;
            targets.push(StatusTarget::plain_http_for_tests(app, &base).unwrap());
            // `d` gets the key of `a`: refused by its own app; `e` has no key and is skipped.
            match app {
                "d" => raw_keys.push("d=key-0".to_owned()),
                "e" => {}
                _ => raw_keys.push(format!("{app}=key-{i}")),
            }
        }
        let keys = StatusKeys::parse(&raw_keys.join(",")).unwrap();
        let poller = StatusPoller::plain_http_for_tests().unwrap();
        let results = poller.poll_all(&targets, &keys, 2).await;
        let apps: Vec<&str> = results.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(apps, ["a", "b", "c", "d"]);
        for (app, r) in &results[..3] {
            assert_eq!(&r.as_ref().unwrap().app, app);
        }
        assert_eq!(results[3].1, Err(StatusFetchError::Status(401)));
        assert_eq!(peak.load(Ordering::SeqCst), 2, "at most 2 at a time");
    }

    #[tokio::test]
    async fn one_poller_fetches_single_targets() {
        let provider = Slow {
            app: "crm",
            now: Arc::default(),
            peak: Arc::default(),
        };
        let base = serve(provider, "key-crm").await;
        let target = StatusTarget::plain_http_for_tests("crm", &base).unwrap();
        let poller = StatusPoller::plain_http_for_tests().unwrap();
        let good = BearerKey::new("key-crm").unwrap();
        let doc = poller.fetch(&target, &good).await.unwrap();
        assert_eq!(doc.app, "crm");
        let wrong = BearerKey::new("other").unwrap();
        assert_eq!(
            poller.fetch(&target, &wrong).await,
            Err(StatusFetchError::Status(401))
        );
        assert!(StatusTarget::plain_http_for_tests("crm", "http://crm.example.com").is_err());
        let https_only = StatusPoller::new().unwrap();
        assert_eq!(
            https_only.fetch(&target, &good).await,
            Err(StatusFetchError::Unreachable),
            "the https poller refuses plain http"
        );
    }

    #[tokio::test]
    async fn no_keys_polls_nothing() {
        let targets = [StatusTarget::new("crm", "https://crm.example.com").unwrap()];
        let out = poll_all(&targets, &StatusKeys::default(), 4).await.unwrap();
        assert!(out.is_empty());
    }
}

//! Polling the status of many apps at once with bounded concurrency. One HTTP client (and connection
//! pool) serves all targets of a call; each target gets only its own key. Scheduling (how often to
//! poll) stays in the app.

use super::client::{StatusClient, StatusClientError, StatusFetchError, StatusTarget, http};
use super::document::StatusDocument;
use super::keys::StatusKeys;
use tokio::task::JoinSet;

/// The outcome of one target: its app name and the document or the error.
pub type PollResult = (String, Result<StatusDocument, StatusFetchError>);

/// Fetches the status of every target that has a key in `keys` (targets without a key are skipped and
/// do not appear in the result), at most `concurrency` at a time (`0` counts as `1`). The results are
/// in the order of `targets`; each fetch has its own [`super::FETCH_DEADLINE`], so a call takes about
/// `ceil(polled / concurrency) × 2 s` at worst. Needs a Tokio runtime (the fetches run as tasks).
pub async fn poll_all(
    targets: &[StatusTarget],
    keys: &StatusKeys,
    concurrency: usize,
) -> Result<Vec<PollResult>, StatusClientError> {
    Ok(poll_with(http(true)?, targets, keys, concurrency).await)
}

/// [`poll_all`] on a given HTTP client (tests use a plain-http one).
async fn poll_with(
    http: reqwest::Client,
    targets: &[StatusTarget],
    keys: &StatusKeys,
    concurrency: usize,
) -> Vec<PollResult> {
    let clients: Vec<(String, StatusClient)> = targets
        .iter()
        .filter_map(|t| {
            let key = keys.get(t.app())?.clone();
            let client = StatusClient::with_http(http.clone(), t.origin(), key);
            Some((t.app().to_owned(), client))
        })
        .collect();
    let mut results: Vec<Option<Result<StatusDocument, StatusFetchError>>> =
        (0..clients.len()).map(|_| None).collect();
    let mut running = JoinSet::new();
    for (i, (_, client)) in clients.iter().enumerate() {
        if running.len() >= concurrency.max(1) {
            collect_one(&mut running, &mut results).await;
        }
        let client = client.clone();
        running.spawn(async move { (i, client.fetch().await) });
    }
    while !running.is_empty() {
        collect_one(&mut running, &mut results).await;
    }
    clients
        .into_iter()
        .zip(results)
        // Every slot is filled: each task either returns or its panic is resumed below.
        .map(|((app, _), r)| (app, r.unwrap_or(Err(StatusFetchError::Unreachable))))
        .collect()
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
            targets.push(StatusTarget::loopback(app, &base));
            // `d` gets the key of `a`: refused by its own app; `e` has no key and is skipped.
            match app {
                "d" => raw_keys.push("d=key-0".to_owned()),
                "e" => {}
                _ => raw_keys.push(format!("{app}=key-{i}")),
            }
        }
        let keys = StatusKeys::parse(&raw_keys.join(",")).unwrap();
        let http = http(false).unwrap();
        let results = poll_with(http, &targets, &keys, 2).await;
        let apps: Vec<&str> = results.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(apps, ["a", "b", "c", "d"]);
        for (app, r) in &results[..3] {
            assert_eq!(&r.as_ref().unwrap().app, app);
        }
        assert_eq!(results[3].1, Err(StatusFetchError::Status(401)));
        assert_eq!(peak.load(Ordering::SeqCst), 2, "at most 2 at a time");
    }

    #[tokio::test]
    async fn no_keys_polls_nothing() {
        let targets = [StatusTarget::new("crm", "https://crm.example.com").unwrap()];
        let out = poll_all(&targets, &StatusKeys::default(), 4).await.unwrap();
        assert!(out.is_empty());
    }
}

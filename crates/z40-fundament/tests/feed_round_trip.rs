//! Both sides of protocol version 1 in one test: a producer (`FeedEndpoint` over `MySqlSource`, served by
//! axum on 127.0.0.1) and a consumer (`FeedConsumer` into a `MySqlStore` replica) on one MariaDB
//! (`TEST_DATABASE_URL`), plus the status figures of the source.
#![cfg(all(
    feature = "feed-source-mysql",
    feature = "replica-mysql",
    feature = "status"
))]
// Test helpers fail fast with unwrap, like the test functions themselves.
#![allow(clippy::unwrap_used)]

mod common;

use chrono::Utc;
use common::{serve, with_schema};
use serde_json::json;
use z40_fundament::consumer::{CursorStore, FeedConsumerBuilder, RunOutcome};
use z40_fundament::credentials::{BearerKey, KeyList, key_entry};
use z40_fundament::feed_source::{FeedEndpoint, source_status};
use z40_fundament::feed_source_mysql::{MySqlSource, OutboxEvent, SourceTables};
use z40_fundament::protocol::Level;
use z40_fundament::replica::{KeepFields, Replica};
use z40_fundament::replica_mysql::{MySqlStore, Tables};
use z40_fundament::status::StatusBuilder;

const CUSTOMER: &str = "urn:example:crm:customer";
const SITE: &str = "urn:example:crm:site";
const FEED_KEY: &str = "feed-key-of-billing-0123456789abcdef";

fn key(n: u64) -> String {
    format!("0192f39e-0000-7000-8000-{n:012}")
}

/// Writes `events` in one app transaction.
async fn write(s: &MySqlSource, events: &[OutboxEvent]) {
    let mut tx = s.pool().begin().await.unwrap();
    for e in events {
        s.write_event(&mut tx, e).await.unwrap();
    }
    tx.commit().await.unwrap();
}

fn customer(n: u64, version: u64) -> OutboxEvent {
    let data = json!({ "name": format!("Customer {n}"), "email": "person@example.com",
                       "address": { "city": "Springfield" } });
    OutboxEvent::state(CUSTOMER, key(n), version, data)
}

#[tokio::test]
async fn producer_and_consumer_fit() {
    let src = SourceTables::with_prefix("crm_").unwrap();
    let rep = Tables::with_prefix("billing_").unwrap();
    let schema = format!("{};\n{}", src.schema(), rep.schema());
    with_schema("round_trip", schema, |pool| async move {
        let source = MySqlSource::new(pool.clone(), src, [CUSTOMER, SITE]).unwrap();
        let clients = KeyList::parse(&key_entry("billing", FEED_KEY).unwrap()).unwrap();
        let base = serve(FeedEndpoint::new(source.clone(), clients.clone()).router()).await;

        // Producer: 5 customers, one deleted, and a site the consumer does not ask for.
        let mut first: Vec<_> = (1..=5).map(|n| customer(n, 1)).collect();
        first.push(OutboxEvent::state(
            SITE,
            key(50),
            1,
            json!({ "name": "Site" }),
        ));
        write(&source, &first).await;
        write(
            &source,
            &[customer(2, 2), OutboxEvent::tombstone(CUSTOMER, key(3), 2)],
        )
        .await;

        // Consumer: pages of 2, so `hasMore` and `nextCursor` carry it through several pages.
        let consumer = FeedConsumerBuilder::plain_http_for_tests(
            "crm",
            &base,
            BearerKey::new(FEED_KEY).unwrap(),
        )
        .unwrap()
        .types([CUSTOMER])
        .page_limit(2)
        .build()
        .unwrap();
        let mapping = KeepFields::new()
            .keep(CUSTOMER, "customer", &["name", "address.city"])
            .unwrap();
        let store = Replica::new(MySqlStore::new(pool.clone(), rep), mapping);

        let r = consumer.run_once(&store).await.unwrap();
        assert_eq!(r.outcome, RunOutcome::Ok);
        assert_eq!(
            (r.pages, r.written, r.cursor),
            (4, 7, 8),
            "the site's cursor is skipped"
        );
        let db = store.store();
        let live = db.records("crm", "customer").await.unwrap();
        let names: Vec<_> = live.iter().map(|r| (r.key.clone(), r.version)).collect();
        assert_eq!(names, [(key(1), 1), (key(2), 2), (key(4), 1), (key(5), 1)]);
        assert_eq!(
            live[1].data,
            Some(json!({ "name": "Customer 2", "address": { "city": "Springfield" } }))
        );
        assert!(
            db.record("crm", "customer", &key(3))
                .await
                .unwrap()
                .unwrap()
                .deleted
        );

        // More changes later; the next run continues from the stored cursor.
        write(&source, &[customer(1, 2), customer(6, 1)]).await;
        let r = consumer.run_once(&store).await.unwrap();
        assert_eq!((r.outcome, r.written, r.cursor), (RunOutcome::Ok, 2, 10));
        assert_eq!(store.load("crm").await.unwrap().unwrap().cursor, 10);

        // Status of the source: offers per type, the consumer's last poll and backlog.
        let doc = source_status(
            StatusBuilder::new("crm", "1.0.0", Utc::now()),
            &source,
            &clients,
            Utc::now(),
        )
        .await
        .unwrap()
        .build();
        assert_eq!(doc.state, Level::Ok);
        let v = serde_json::to_value(&doc).unwrap();
        assert_eq!(v["angebote"][0]["typ"], CUSTOMER);
        assert_eq!(v["angebote"][0]["letzterCursor"], 10);
        assert_eq!(v["angebote"][1]["ereignisse24h"], 1);
        assert_eq!(v["abrufer"][0]["name"], "billing");
        assert_eq!(v["abrufer"][0]["cursor"], 10);
        assert_eq!(v["abrufer"][0]["rueckstand"], 0);
        assert_eq!(v["abrufer"][0]["fehlversuche24h"], json!(null));

        // A wrong key is refused and counted (in the background) as a note of the status.
        let wrong = FeedConsumerBuilder::plain_http_for_tests(
            "crm",
            &base,
            BearerKey::new("wrong").unwrap(),
        )
        .unwrap()
        .build()
        .unwrap();
        let r = wrong.run_once(&store).await.unwrap();
        assert!(matches!(r.outcome, RunOutcome::Failed(_)));
        let mut note = None;
        for _ in 0..50 {
            let doc = source_status(
                StatusBuilder::new("crm", "1", Utc::now()),
                &source,
                &clients,
                Utc::now(),
            )
            .await
            .unwrap()
            .build();
            note = doc.notes.first().map(|n| n.text.clone());
            if note.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(note.as_deref(), Some("1 Fehlversuch am Feed in 24 Stunden"));
    })
    .await;
}

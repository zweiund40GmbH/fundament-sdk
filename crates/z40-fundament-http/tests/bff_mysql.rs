//! `MySqlSessionStore` against MariaDB (`TEST_DATABASE_URL`): the store operations, that only hashes and
//! sealed values reach the database, the cleanup, and a full login round trip over it.
#![cfg(feature = "bff-mysql")]
#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{Method, StatusCode};
use chrono::{SubsecRound, TimeDelta, Utc};
use common::app::TestApp;
use common::db::with_schema;
use sqlx::MySqlPool;
use z40_fundament_http::bff::{PendingLogin, SessionRecord, SessionStore, TokenHash};
use z40_fundament_http::bff_mysql::{MySqlSessionStore, SessionTables};

fn tables() -> SessionTables {
    SessionTables::with_prefix("app_").unwrap()
}

fn record(sub: &str, sid: Option<&str>, ends_in: i64) -> SessionRecord {
    let now = Utc::now().trunc_subsecs(3);
    SessionRecord {
        sub: sub.to_owned(),
        sid: sid.map(str::to_owned),
        data: r#"{"name":"Erika"}"#.to_owned(),
        created_at: now,
        last_seen_at: now,
        expires_at: now + TimeDelta::seconds(ends_in),
    }
}

#[tokio::test]
async fn store_operations() {
    with_schema(
        "bff_store",
        tables().schema(),
        |pool: MySqlPool| async move {
            let store = MySqlSessionStore::new(pool, tables());
            let (a, b, c) = (TokenHash::of("a"), TokenHash::of("b"), TokenHash::of("c"));
            let rec = record("alice", Some("s1"), 600);
            store.create(&a, &rec).await.unwrap();
            assert_eq!(store.load(&a).await.unwrap().unwrap(), rec);
            assert!(store.load(&b).await.unwrap().is_none());
            let later = rec.last_seen_at + TimeDelta::seconds(30);
            store
                .touch(&a, later, later + TimeDelta::seconds(600))
                .await
                .unwrap();
            let touched = store.load(&a).await.unwrap().unwrap();
            assert_eq!(
                (touched.last_seen_at, touched.created_at),
                (later, rec.created_at)
            );
            store
                .create(&b, &record("alice", Some("s2"), 600))
                .await
                .unwrap();
            store
                .create(&c, &record("ALICE", Some("S1"), 600))
                .await
                .unwrap();
            assert_eq!(
                store.delete_by_sid("s1").await.unwrap(),
                1,
                "exact, not case-insensitive"
            );
            store
                .create(&TokenHash::of("d"), &record("alice", None, 600))
                .await
                .unwrap();
            assert_eq!(store.delete_by_sub_without_sid("alice").await.unwrap(), 1);
            assert_eq!(store.delete_by_sub("alice").await.unwrap(), 1);
            assert!(store.delete(&c).await.unwrap());
            assert!(!store.delete(&c).await.unwrap());

            let login = PendingLogin {
                state: vec![9; 40],
                return_to: "/orders".to_owned(),
                expires_at: Utc::now().trunc_subsecs(3) + TimeDelta::seconds(600),
            };
            store.put_login(&a, &login).await.unwrap();
            let (first, second) = tokio::join!(store.take_login(&a), store.take_login(&a));
            let got: Vec<_> = [first.unwrap(), second.unwrap()]
                .into_iter()
                .flatten()
                .collect();
            assert_eq!(got, [login], "single use, also concurrently");
        },
    )
    .await;
}

#[tokio::test]
async fn cleanup_removes_only_ended_rows() {
    with_schema(
        "bff_cleanup",
        tables().schema(),
        |pool: MySqlPool| async move {
            let store = MySqlSessionStore::new(pool.clone(), tables());
            store
                .create(&TokenHash::of("old"), &record("u", None, -1))
                .await
                .unwrap();
            store
                .create(&TokenHash::of("new"), &record("u", None, 600))
                .await
                .unwrap();
            let ended = PendingLogin {
                state: vec![1],
                return_to: "/".to_owned(),
                expires_at: Utc::now() - TimeDelta::seconds(1),
            };
            store.put_login(&TokenHash::of("l"), &ended).await.unwrap();
            assert_eq!(store.delete_expired(Utc::now()).await.unwrap(), 2);
            assert!(store.load(&TokenHash::of("new")).await.unwrap().is_some());
            assert_eq!(store.delete_expired(Utc::now()).await.unwrap(), 0);
        },
    )
    .await;
}

#[tokio::test]
async fn login_round_trip_stores_only_hashes_and_sealed_values() {
    with_schema(
        "bff_round_trip",
        tables().schema(),
        |pool: MySqlPool| async move {
            let store = MySqlSessionStore::new(pool.clone(), tables());
            let app = TestApp::with_store(store, &[]).await;
            let (authorize, login_cookie) = app.start_login("/orders").await;
            let login_value = login_cookie.split_once('=').unwrap().1.to_owned();
            let state: Vec<u8> = sqlx::query_scalar("SELECT `state` FROM `app_bff_login`")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert!(
                !String::from_utf8_lossy(&state).contains("pkce_verifier"),
                "sealed"
            );
            let a = app
                .provider
                .authorize(&authorize, "user-1", Some("sid-1"), &["admin"], |_| {});
            let res = app
                .send(
                    Method::GET,
                    &format!("/api/auth/callback?{}", a.query),
                    &login_cookie,
                    &[],
                    "",
                )
                .await;
            assert_eq!(res.status(), StatusCode::SEE_OTHER);
            let session = common::app::session_cookie(res.headers()).unwrap();
            let value = session.split_once('=').unwrap().1.to_owned();

            type Row = (Vec<u8>, String, Option<String>, String);
            let rows: Vec<Row> =
                sqlx::query_as("SELECT `id_hash`, `sub`, `sid`, `data` FROM `app_bff_session`")
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            assert_eq!(rows.len(), 1);
            let (id_hash, sub, sid, data) = &rows[0];
            assert_eq!(id_hash.as_slice(), TokenHash::of(&value).as_bytes());
            assert_eq!((sub.as_str(), sid.as_deref()), ("user-1", Some("sid-1")));
            for stored in [data.as_bytes(), id_hash.as_slice()] {
                let text = String::from_utf8_lossy(stored);
                assert!(!text.contains(&value) && !text.contains(&login_value));
            }
            assert!(!data.contains("eyJ"), "no token stored");
            let logins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM `app_bff_login`")
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(logins, 0);

            let me = app.send(Method::GET, "/api/me", &session, &[], "").await;
            assert_eq!(me.status(), StatusCode::OK);
            let token = app.provider.logout_token(None, Some("sid-1"));
            let body = format!("logout_token={token}");
            let form = ("content-type", "application/x-www-form-urlencoded");
            let res = app
                .send(
                    Method::POST,
                    "/api/auth/backchannel-logout",
                    "",
                    &[form],
                    &body,
                )
                .await;
            assert_eq!(res.status(), StatusCode::OK);
            let me = app.send(Method::GET, "/api/me", &session, &[], "").await;
            assert_eq!(me.status(), StatusCode::UNAUTHORIZED);
        },
    )
    .await;
}

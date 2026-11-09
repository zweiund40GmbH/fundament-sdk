//! Classification of database errors: by number, and against a real MariaDB (`TEST_DATABASE_URL`, an
//! own database; `[skip]` without it) for a duplicate key and a lock wait timeout.

use super::*;
use sqlx::{Executor, MySqlPool};

#[test]
fn numbers_and_connection_errors() {
    assert_eq!(kind_of_number(1213), StoreErrorKind::Retryable);
    assert_eq!(kind_of_number(1205), StoreErrorKind::Retryable);
    assert_eq!(kind_of_number(1062), StoreErrorKind::Conflict);
    assert_eq!(kind_of_number(1146), StoreErrorKind::Other);
    let io = sqlx::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset));
    assert!(db(io).is_retryable());
    assert!(db(sqlx::Error::PoolTimedOut).is_retryable());
    assert_eq!(db(sqlx::Error::RowNotFound).kind(), StoreErrorKind::Other);
}

#[tokio::test]
async fn refused_connection_is_retryable() {
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect_lazy("mysql://x@127.0.0.1:1/x")
        .unwrap();
    let e = sqlx::query("SELECT 1").execute(&pool).await.unwrap_err();
    assert!(db(e).is_retryable());
}

/// Runs `test` on a pool of a fresh database with the table `t (id INT PRIMARY KEY)`.
async fn with_table<F, Fut>(name: &str, test: F)
where
    F: FnOnce(MySqlPool) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let Ok(server) = std::env::var("TEST_DATABASE_URL") else {
        println!("[skip] {name}: TEST_DATABASE_URL not set");
        return;
    };
    let server = server.trim_end_matches('/').to_owned();
    let db_name = format!("fundament_sdk_{name}_{}", std::process::id());
    let admin = MySqlPool::connect(&server).await.unwrap();
    admin
        .execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE `{db_name}`")))
        .await
        .unwrap();
    let url = format!("{server}/{db_name}");
    let result = tokio::spawn(async move {
        let pool = MySqlPool::connect(&url).await.unwrap();
        pool.execute("CREATE TABLE t (id INT PRIMARY KEY) ENGINE=InnoDB")
            .await
            .unwrap();
        test(pool.clone()).await;
        pool.close().await;
    })
    .await;
    admin
        .execute(sqlx::AssertSqlSafe(format!("DROP DATABASE `{db_name}`")))
        .await
        .unwrap();
    if let Err(e) = result {
        std::panic::resume_unwind(e.into_panic());
    }
}

#[tokio::test]
async fn duplicate_key_and_lock_wait_timeout() {
    with_table("store_error_kinds", |pool| async move {
        pool.execute("INSERT INTO t VALUES (1)").await.unwrap();
        let dup = pool.execute("INSERT INTO t VALUES (1)").await.unwrap_err();
        assert_eq!(error_number(&dup), Some(ER_DUP_ENTRY));
        assert_eq!(db(dup).kind(), StoreErrorKind::Conflict);

        // A holds the row lock; B waits at most one second for it.
        let mut a = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM t WHERE id = 1 FOR UPDATE")
            .fetch_one(&mut *a)
            .await
            .unwrap();
        let mut b = pool.acquire().await.unwrap();
        b.execute("SET SESSION innodb_lock_wait_timeout = 1")
            .await
            .unwrap();
        let wait = b
            .execute("UPDATE t SET id = 2 WHERE id = 1")
            .await
            .unwrap_err();
        assert_eq!(error_number(&wait), Some(ER_LOCK_WAIT_TIMEOUT));
        assert!(db(wait).is_retryable());
        a.rollback().await.unwrap();
    })
    .await;
}

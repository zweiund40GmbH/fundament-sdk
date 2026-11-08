//! A database per test on the server of `TEST_DATABASE_URL` (dropped afterwards, also on panic).

use sqlx::{Executor, MySqlPool};
use std::future::Future;

/// Runs `test` with a pool on a fresh database that has `schema`. Without `TEST_DATABASE_URL` prints
/// `[skip]`.
pub async fn with_schema<F, Fut>(name: &str, schema: String, test: F)
where
    F: FnOnce(MySqlPool) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let Ok(server) = std::env::var("TEST_DATABASE_URL") else {
        println!("[skip] {name}: TEST_DATABASE_URL not set");
        return;
    };
    let server = server.trim_end_matches('/').to_owned();
    let db = format!("fundament_sdk_http_{name}_{}", std::process::id());
    let admin = MySqlPool::connect(&server).await.unwrap();
    let drop = sqlx::AssertSqlSafe(format!("DROP DATABASE IF EXISTS `{db}`"));
    admin.execute(drop).await.unwrap();
    let create = sqlx::AssertSqlSafe(format!("CREATE DATABASE `{db}`"));
    admin.execute(create).await.unwrap();
    let url = format!("{server}/{db}");
    let result = tokio::spawn(async move {
        let pool = MySqlPool::connect(&url).await.unwrap();
        let without_comments: String = schema
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        for stmt in without_comments
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            pool.execute(sqlx::AssertSqlSafe(stmt.to_owned()))
                .await
                .unwrap();
        }
        test(pool.clone()).await;
        pool.close().await;
    })
    .await;
    let drop = sqlx::AssertSqlSafe(format!("DROP DATABASE `{db}`"));
    admin.execute(drop).await.unwrap();
    if let Err(e) = result {
        std::panic::resume_unwind(e.into_panic());
    }
}

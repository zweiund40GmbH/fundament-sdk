//! The windowed delete of the retention uses the primary key, not a scan of the whole outbox
//! (`TEST_DATABASE_URL`, an own database; `[skip]` without it).

use super::*;
use crate::feed_source_mysql::SourceTables;
use sqlx::{Executor, MySqlPool, Row};

/// The `type` column of every row of `EXPLAIN` for `sql`.
async fn plan_types(pool: &MySqlPool, sql: String) -> Vec<(String, String)> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("EXPLAIN {sql}")))
        .bind(1000u64)
        .bind(2000u64)
        .bind(1000u64)
        .bind(2000u64)
        .bind(Utc::now().naive_utc())
        .fetch_all(pool)
        .await
        .unwrap();
    rows.iter()
        .map(|r| {
            let table: Option<String> = r.try_get("table").unwrap();
            let kind: Option<String> = r.try_get("type").unwrap();
            (table.unwrap_or_default(), kind.unwrap_or_default())
        })
        .collect()
}

#[tokio::test]
async fn windowed_delete_uses_the_primary_key() {
    let Ok(server) = std::env::var("TEST_DATABASE_URL") else {
        println!("[skip] windowed_delete_uses_the_primary_key: TEST_DATABASE_URL not set");
        return;
    };
    let server = server.trim_end_matches('/').to_owned();
    let name = format!("fundament_sdk_purge_plan_{}", std::process::id());
    let admin = MySqlPool::connect(&server).await.unwrap();
    admin
        .execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE `{name}`")))
        .await
        .unwrap();
    let url = format!("{server}/{name}");
    let result = tokio::spawn(async move {
        let pool = MySqlPool::connect(&url).await.unwrap();
        let tables = SourceTables::with_prefix("").unwrap();
        let schema: String = tables
            .schema()
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        for stmt in schema.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            pool.execute(sqlx::AssertSqlSafe(stmt.to_owned()))
                .await
                .unwrap();
        }
        // 5000 rows over 50 records, so the optimizer has a real table to plan for.
        pool.execute(sqlx::AssertSqlSafe(
            "INSERT INTO fundament_outbox (`type`, `record_key`, `version`, `occurred_at`, `data`, `cursor`) \
             SELECT 'urn:example:crm:customer', \
                    CONCAT('0192f39e-0000-7000-8000-', LPAD(seq MOD 50, 12, '0')), seq DIV 50 + 1, \
                    UTC_TIMESTAMP(3) - INTERVAL 40 DAY, '{}', seq + 1 FROM seq_0_to_4999"
                .to_owned(),
        ))
        .await
        .unwrap();
        pool.execute("ANALYZE TABLE fundament_outbox").await.unwrap();
        let plan = plan_types(&pool, tables.sql(PURGE_WINDOW).0).await;
        assert!(!plan.is_empty());
        assert!(
            plan.iter().all(|(_, kind)| kind != "ALL"),
            "no full scan in the windowed delete: {plan:?}"
        );
        pool.close().await;
    })
    .await;
    admin
        .execute(sqlx::AssertSqlSafe(format!("DROP DATABASE `{name}`")))
        .await
        .unwrap();
    if let Err(e) = result {
        std::panic::resume_unwind(e.into_panic());
    }
}

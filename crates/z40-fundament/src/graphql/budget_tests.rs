//! Tests of the read budget and the read limiter.

use super::*;
use std::time::Duration;

#[tokio::test]
async fn refuses_beyond_the_cap_without_running_the_read() {
    let budget = ReadBudget::new(10, ReadLimiter::new(3));
    let ran = AtomicUsize::new(0);
    let mut refused = Vec::new();
    for _ in 0..15 {
        let r = budget
            .read(async {
                ran.fetch_add(1, Ordering::Relaxed);
            })
            .await;
        if let Err(e) = r {
            refused.push(e);
        }
    }
    assert_eq!(refused.len(), 5);
    assert_eq!(ran.load(Ordering::Relaxed), 10);
    assert_eq!(budget.reads(), 10);
    assert_eq!(budget.cap(), 10);
    let e = &refused[0];
    assert_eq!(e.message, "Zu viele Lesezugriffe in einer Anfrage.");
    let code = e.extensions.as_ref().and_then(|x| x.get("code")).cloned();
    assert_eq!(code, Some(async_graphql::Value::from("VALIDATION")));
}

#[tokio::test]
async fn clones_share_the_count_and_the_output_is_passed_on() {
    let budget =
        ReadBudget::from_limits(&SubgraphLimits::default().reads(2), ReadLimiter::default());
    let other = budget.clone();
    assert_eq!(budget.read(async { Ok::<_, ()>(7) }).await.unwrap(), Ok(7));
    assert_eq!(
        other.read(async { Err::<u8, _>("db") }).await.unwrap(),
        Err("db")
    );
    assert!(budget.read(async {}).await.is_err());
    assert_eq!(other.reads(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn never_more_than_the_permits_at_once() {
    let limiter = ReadLimiter::from_limits(&SubgraphLimits::default());
    let now = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    // Several requests (own budgets) sharing the limiter, each with many concurrent reads.
    for _ in 0..4 {
        let budget = ReadBudget::new(100, limiter.clone());
        for _ in 0..10 {
            let (b, n, p) = (budget.clone(), Arc::clone(&now), Arc::clone(&peak));
            tasks.push(tokio::spawn(async move {
                b.read(async {
                    let k = n.fetch_add(1, Ordering::SeqCst) + 1;
                    p.fetch_max(k, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    n.fetch_sub(1, Ordering::SeqCst);
                })
                .await
            }));
        }
    }
    for t in tasks {
        assert!(t.await.unwrap().is_ok());
    }
    assert_eq!(peak.load(Ordering::SeqCst), 3);
    assert_eq!(limiter.available(), 3);
}

#[tokio::test]
async fn a_cancelled_read_releases_its_permit() {
    let limiter = ReadLimiter::new(1);
    let budget = ReadBudget::new(10, limiter.clone());
    let slow = budget.read(tokio::time::sleep(Duration::from_secs(5)));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), slow)
            .await
            .is_err()
    );
    assert_eq!(limiter.available(), 1);
    assert!(budget.read(async {}).await.is_ok());
    assert_eq!(
        ReadLimiter::new(0).available(),
        1,
        "0 permits would block every read"
    );
}

#[tokio::test]
async fn from_ctx_finds_the_budget_of_the_request() {
    use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema};

    struct Query;
    #[Object]
    impl Query {
        async fn reads(&self, ctx: &Context<'_>) -> async_graphql::Result<usize> {
            let b = ReadBudget::from_ctx(ctx)?;
            b.read(async {}).await?;
            Ok(b.reads())
        }
    }
    let schema = Schema::new(Query, EmptyMutation, EmptySubscription);
    let budget = ReadBudget::new(5, ReadLimiter::default());
    let res = schema
        .execute(async_graphql::Request::new("{ reads }").data(budget.clone()))
        .await;
    assert_eq!(
        res.data.into_json().unwrap(),
        serde_json::json!({ "reads": 1 })
    );
    assert_eq!(budget.reads(), 1);

    let res = schema.execute("{ reads }").await;
    assert_eq!(res.errors[0].message, MSG_INTERNAL);
}

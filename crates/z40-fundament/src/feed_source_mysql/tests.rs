use super::*;

#[test]
fn table_names_and_schema() {
    let t = SourceTables::with_prefix("app_").unwrap();
    assert_eq!(t.outbox, "app_fundament_outbox");
    assert_eq!(t.lock, "app_fundament-cursor");
    let s = t.schema();
    for name in ["outbox", "outbox_counter", "poll", "poll_failure"] {
        assert!(
            s.contains(&format!(
                "CREATE TABLE IF NOT EXISTS `app_fundament_{name}`"
            )),
            "{name}"
        );
    }
    assert!(!s.contains("`fundament_"), "every table prefixed");
    assert!(s.contains("INSERT IGNORE INTO `app_fundament_outbox_counter`"));
    assert_eq!(
        t.sql("SELECT * FROM {outbox} JOIN {counter} JOIN {poll} JOIN {failure}")
            .0,
        "SELECT * FROM `app_fundament_outbox` JOIN `app_fundament_outbox_counter` \
         JOIN `app_fundament_poll` JOIN `app_fundament_poll_failure`"
    );
    assert!(SourceTables::with_prefix("").is_ok());
    for bad in ["a-b", "x`; DROP", "toolongprefix_____"] {
        assert_eq!(
            SourceTables::with_prefix(bad),
            Err(InvalidTableName),
            "{bad}"
        );
    }
    assert!(SourceTables::new("o", "", "p", "f").is_err());
    let custom = SourceTables::new("o", "c", "p", "f").unwrap();
    assert_eq!(custom.lock, "o-cursor");
    assert!(custom.schema().contains("CREATE TABLE IF NOT EXISTS `c`"));
}

#[tokio::test]
async fn types_are_checked() {
    let pool = MySqlPool::connect_lazy("mysql://x@127.0.0.1:1/x").unwrap();
    let t = SourceTables::with_prefix("").unwrap();
    let s = MySqlSource::new(pool.clone(), t.clone(), ["urn:example:crm:customer"]).unwrap();
    assert_eq!(s.types, ["urn:example:crm:customer"]);
    assert_eq!(
        MySqlSource::new(pool.clone(), t.clone(), ["urn:a", "urn:a"]).unwrap_err(),
        InvalidType
    );
    assert!(MySqlSource::new(pool, t, ["customer"]).is_err());
}

#[tokio::test]
async fn events_are_checked_before_writing() {
    let pool = MySqlPool::connect_lazy("mysql://x@127.0.0.1:1/x").unwrap();
    let s = MySqlSource::new(
        pool,
        SourceTables::with_prefix("").unwrap(),
        ["urn:example:crm:customer"],
    )
    .unwrap();
    let k = "0192F3A1-7C2E-7000-8000-00000000002A";
    let t = "urn:example:crm:customer";
    let data = serde_json::json!({ "name": "A" });
    assert_eq!(s.check(&OutboxEvent::state(t, k, 1, data.clone())), Ok(()));
    assert_eq!(s.check(&OutboxEvent::tombstone(t, k, 2)), Ok(()));
    for (e, why) in [
        (
            OutboxEvent::state("urn:example:x:y", k, 1, data.clone()),
            InvalidEvent::UnknownType,
        ),
        (
            OutboxEvent::state(t, "17", 1, data.clone()),
            InvalidEvent::InvalidKey,
        ),
        (
            OutboxEvent::state(t, k, 0, data),
            InvalidEvent::InvalidVersion,
        ),
        (
            OutboxEvent::state(t, k, 1, serde_json::json!([1])),
            InvalidEvent::MissingData,
        ),
    ] {
        assert_eq!(s.check(&e), Err(why));
    }
}

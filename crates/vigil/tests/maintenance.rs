mod common;
use common::*;

#[tokio::test]
async fn prunes_only_old_checks() {
    let (pool, _d) = fresh_pool().await;
    sqlx::query("INSERT INTO monitors (name,type,url,expected_status_codes,interval_seconds,timeout_seconds,\
        confirmation_threshold,recovery_threshold,retry_interval_seconds,status,created_at,updated_at)\
        VALUES ('m','http','https://x','200-299',300,30,3,1,30,'up',0,0)").execute(&pool).await.unwrap();
    let now = 40*86400i64;
    sqlx::query("INSERT INTO checks (monitor_id,checked_at,status) VALUES (1,0,'down')").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO checks (monitor_id,checked_at,status) VALUES (1,?,'up')").bind(now-86400).execute(&pool).await.unwrap();
    let removed = vigil::maintenance::prune_old_checks(&pool, 30, now).await.unwrap();
    assert_eq!(removed, 1);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM checks").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
}

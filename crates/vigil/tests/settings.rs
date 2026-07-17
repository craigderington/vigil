mod common;
use common::*;

#[tokio::test]
async fn settings_default_and_roundtrip() {
    let (pool, _d) = fresh_pool().await;
    assert_eq!(vigil::settings_store::cooldown_minutes(&pool).await, 15);
    vigil::settings_store::set(&pool, "notify.cooldown_minutes", "30").await.unwrap();
    assert_eq!(vigil::settings_store::cooldown_minutes(&pool).await, 30);
}

use vigil::models::Connectivity::*; use std::sync::{Arc, atomic::{AtomicBool,AtomicUsize,Ordering}};
fn bus() -> vigil::events::Bus { tokio::sync::broadcast::channel(16).0 }
#[test] fn verdict_maps() {
    assert_eq!(vigil::anchor::verdict_from_probe(true), Online);
    assert_eq!(vigil::anchor::verdict_from_probe(false), Offline);
}
#[tokio::test] async fn flips_offline_then_online() {
    let reachable = Arc::new(AtomicBool::new(false)); let r = reachable.clone();
    let g = Arc::new(vigil::anchor::AnchorGate::with_prober(bus(), Box::new(move || r.load(Ordering::SeqCst))));
    assert_eq!(g.probe_and_update().await, Some(false)); // edge -> offline
    assert_eq!(g.current().await, Offline);
    reachable.store(true, Ordering::SeqCst);
    assert_eq!(g.probe_and_update().await, Some(true));  // edge -> online
    assert_eq!(g.current().await, Online);
}
#[tokio::test] async fn caches_within_ttl() {
    let calls = Arc::new(AtomicUsize::new(0)); let c = calls.clone();
    let g = vigil::anchor::AnchorGate::with_prober(bus(), Box::new(move || { c.fetch_add(1,Ordering::SeqCst); true }));
    let _ = g.current().await; let _ = g.current().await; // 2nd reuses cache within 10s
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

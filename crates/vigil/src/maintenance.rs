//! Maintenance-window enforcement. Stubbed for P1 wiring (Task 14) so
//! `main::serve` has a stable background task to spawn; Task 19 fills in
//! actual window scope/suppression logic (§8 of the spec).

pub async fn run(_state: crate::app::AppState) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}

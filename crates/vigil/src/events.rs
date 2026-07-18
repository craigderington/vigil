use serde::Serialize;
use crate::models::*;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    MonitorUpdated { id: i64, status: Status, response_time_ms: Option<i64>, checked_at: Ts },
    MonitorTransition { id: i64, from: Status, to: Status, incident_id: Option<i64> },
    IncidentOpened { id: i64, monitor_id: i64 },
    IncidentResolved { id: i64, monitor_id: i64, duration_seconds: i64 },
    ConnectivityChanged { online: bool },
    Snapshot { monitors: Vec<Monitor>, online: bool },
    CertUpdated { id: i64 },
}
pub type Bus = tokio::sync::broadcast::Sender<Event>;

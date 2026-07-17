use crate::events::{Bus, Event};
use crate::models::Connectivity;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TTL_SECONDS: i64 = 10;
const POLL_INTERVAL_SECONDS: u64 = 15;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Pure mapping from a raw probe result to a connectivity verdict.
pub fn verdict_from_probe(any_anchor_reachable: bool) -> Connectivity {
    if any_anchor_reachable {
        Connectivity::Online
    } else {
        Connectivity::Offline
    }
}

struct Inner {
    verdict: Option<Connectivity>,
    checked_at: i64,
}

/// The internet-sanity gate: probes a set of known-good anchor hosts and
/// caches the verdict (with a TTL) so monitor evaluation can cheaply ask
/// "is it us, or is it them?" before declaring a monitor DOWN.
pub struct AnchorGate {
    inner: Mutex<Inner>,
    prober: Arc<dyn Fn() -> bool + Send + Sync>,
    hosts: Arc<Mutex<Vec<String>>>,
    bus: Bus,
}

impl AnchorGate {
    /// Builds a gate with a real TCP-connect prober over `hosts` (each
    /// `host:port`). Hosts are read live from the shared vec on every probe,
    /// so `set_hosts` takes effect without rebuilding the gate.
    pub fn new(hosts: Vec<String>, bus: Bus) -> Self {
        let hosts = Arc::new(Mutex::new(hosts));
        let hosts_for_prober = hosts.clone();
        let prober: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            let current_hosts = hosts_for_prober
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            for host in current_hosts {
                let addrs = match host.to_socket_addrs() {
                    Ok(addrs) => addrs,
                    Err(_) => continue,
                };
                for addr in addrs {
                    if TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).is_ok() {
                        return true;
                    }
                }
            }
            false
        });
        AnchorGate {
            inner: Mutex::new(Inner { verdict: None, checked_at: 0 }),
            prober,
            hosts,
            bus,
        }
    }

    /// Test/inversion-of-control constructor: supply a fake prober directly.
    pub fn with_prober(bus: Bus, prober: Box<dyn Fn() -> bool + Send + Sync>) -> Self {
        AnchorGate {
            inner: Mutex::new(Inner { verdict: None, checked_at: 0 }),
            prober: Arc::from(prober),
            hosts: Arc::new(Mutex::new(Vec::new())),
            bus,
        }
    }

    /// Replace the anchor host list (`host:port` strings) used by the real prober.
    pub fn set_hosts(&self, hosts: Vec<String>) {
        *self.hosts.lock().unwrap_or_else(|e| e.into_inner()) = hosts;
    }

    /// Runs the prober off the async runtime, computes the new verdict, and
    /// — only on a change from the previously stored verdict (including the
    /// very first probe) — persists it and emits `ConnectivityChanged`.
    /// Returns `Some(online)` on an edge, `None` if the verdict is unchanged.
    pub async fn probe_and_update(&self) -> Option<bool> {
        let p = self.prober.clone();
        let reachable = tokio::task::spawn_blocking(move || p()).await.unwrap_or(false);
        let new = verdict_from_probe(reachable);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let changed = inner.verdict != Some(new);
        inner.checked_at = now();
        if changed {
            inner.verdict = Some(new);
            drop(inner);
            let online = matches!(new, Connectivity::Online);
            let _ = self.bus.send(Event::ConnectivityChanged { online });
            Some(online)
        } else {
            None
        }
    }

    /// Returns the cached verdict, re-probing first if it's stale (older
    /// than the TTL). Fail-open: if there's no verdict yet after probing,
    /// default to Online rather than inventing an outage.
    pub async fn current(&self) -> Connectivity {
        let checked_at = self.inner.lock().unwrap_or_else(|e| e.into_inner()).checked_at;
        if now() - checked_at > TTL_SECONDS {
            self.probe_and_update().await;
        }
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .verdict
            .unwrap_or(Connectivity::Online)
    }

    /// Background poller: re-probes every 15s for the lifetime of the app.
    pub async fn run_poller(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECONDS)).await;
            self.probe_and_update().await;
        }
    }
}

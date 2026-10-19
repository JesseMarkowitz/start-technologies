//! Whether a management tunnel is alive, and what each end does about it
//! (design D18, D25).
//!
//! The Core opens a trunk port only while an adopted access point's tunnel
//! is alive through it, re-pins the endpoint it last learned so it can
//! initiate a handshake itself, and keeps the access point's path to the
//! update source resolved. An access point whose tunnel is not alive takes
//! its SSIDs down and cuts its access ports, and brings them back once the
//! tunnel has held for a moment.
//!
//! Liveness is traffic received. Each end sends the other an echo every
//! tick, so a tunnel that is up receives something every few seconds from
//! a peer that is really there. WireGuard's own keepalive cannot be relied
//! on for this: a peer re-arms its keepalive timer on every packet it
//! receives, so of two peers with the same interval one ends up sending all
//! the keepalives and the other none, and the sender sees silence. Handshake
//! age alone would not do either, since a busy session rekeys only every
//! two to three minutes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use crate::satellite::{self, RouterRole};

pub const TICK: Duration = Duration::from_secs(5);
/// One keepalive may be lost; two in a row is dark.
pub const THRESHOLD: Duration = Duration::from_secs(45);
/// A resumed tunnel must hold this long before the access point reopens.
pub const RESUME_AFTER: Duration = Duration::from_secs(10);
/// A session is renewed at least this often while traffic flows; an older
/// handshake on first sight says nothing about the present.
const HANDSHAKE_FRESH: u64 = 180;
const PERSIST_EVERY: Duration = Duration::from_secs(60);

/// One reading of `wg show <interface> dump` for a tunnel's single peer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    pub endpoint: Option<String>,
    /// Unix seconds; 0 before any handshake.
    pub latest_handshake: u64,
    pub rx_bytes: u64,
}

/// The first peer line; the first line is the interface itself.
pub fn parse_dump(dump: &str) -> Option<Observation> {
    let fields: Vec<&str> = dump.lines().nth(1)?.split('\t').collect();
    if fields.len() < 7 {
        return None;
    }
    Some(Observation {
        endpoint: Some(fields[2])
            .filter(|e| *e != "(none)" && !e.is_empty())
            .map(str::to_string),
        latest_handshake: fields[4].parse().unwrap_or(0),
        rx_bytes: fields[5].parse().unwrap_or(0),
    })
}

/// Judges one tunnel across readings.
#[derive(Default)]
pub struct Monitor {
    last: Option<Observation>,
    progressed_at: Option<Instant>,
}

impl Monitor {
    /// Live while packets keep arriving: a changed receive counter or a new
    /// handshake within the threshold. A missing interface is dead.
    pub fn observe(&mut self, obs: Option<&Observation>, now: Instant, now_unix: u64) -> bool {
        match (obs, &self.last) {
            (Some(o), Some(l)) => {
                if o.rx_bytes != l.rx_bytes || o.latest_handshake != l.latest_handshake {
                    self.progressed_at = Some(now);
                }
            }
            (Some(o), None) => {
                if o.latest_handshake > 0
                    && now_unix.saturating_sub(o.latest_handshake) < HANDSHAKE_FRESH
                {
                    self.progressed_at = Some(now);
                }
            }
            (None, _) => self.progressed_at = None,
        }
        self.last = obs.cloned();
        self.progressed_at
            .is_some_and(|t| now.duration_since(t) < THRESHOLD)
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn observe_interface(interface: &str) -> Option<Observation> {
    command_output("wg", &["show", interface, "dump"])
        .await
        .and_then(|d| parse_dump(&d))
}

/// One echo to the peer's tunnel address, not waited for: the reply, and the
/// peer's own echoes, are what the next readings count.
fn probe(addr: &str) {
    let addr = addr.to_string();
    tokio::spawn(async move {
        let _ = crate::run_quiet_async(
            tokio::process::Command::new("ping").args(["-c", "1", "-W", "2", "-q", &addr]),
        )
        .await;
    });
}

static RUNNING: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

/// Starts the loop this router's role calls for, once. On its own thread:
/// adoption calls this from a runtime that ends with the exchange.
pub fn start() {
    let role = satellite::load_role();
    let run: fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> = match role.role {
        RouterRole::Core => || Box::pin(run_core()),
        RouterRole::Satellite if role.core_key.is_some() => || Box::pin(run_ap()),
        RouterRole::Satellite => return,
    };
    {
        let mut running = RUNNING.lock().unwrap();
        if *running {
            return;
        }
        *running = true;
    }
    if let Err(e) = std::thread::Builder::new()
        .name("liveness".into())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("liveness runtime")
                .block_on(run());
            *RUNNING.lock().unwrap() = false;
        })
    {
        tracing::error!("liveness: failed to start thread: {e}");
        *RUNNING.lock().unwrap() = false;
    }
}

// ── Core ────────────────────────────────────────────────────────────────────

/// `[<ip>%<scope>]:<port>` or `<ip>:<port>` as `wg` prints it.
pub fn endpoint_host(endpoint: &str) -> Option<(&str, Option<&str>)> {
    let inner = endpoint
        .strip_prefix('[')
        .and_then(|e| e.split_once(']'))
        .map(|(host, _)| host)
        .or_else(|| endpoint.rsplit_once(':').map(|(host, _)| host))?;
    Some(match inner.split_once('%') {
        Some((ip, scope)) => (ip, Some(scope)),
        None => (inner, None),
    })
}

/// `ip -6 neigh show dev <scope>`: the link-layer address of `ip`.
pub fn neighbour_mac(neigh: &str, ip: &str) -> Option<String> {
    neigh.lines().find_map(|line| {
        let mut w = line.split_whitespace();
        (w.next()? == ip).then(|| ())?;
        let w: Vec<&str> = w.collect();
        w.iter()
            .position(|t| *t == "lladdr")
            .and_then(|i| w.get(i + 1))
            .map(|m| m.to_string())
    })
}

/// `bridge fdb show br <bridge> vlan <id>`: the port that learned `mac`.
pub fn fdb_port(fdb: &str, mac: &str) -> Option<String> {
    fdb.lines().find_map(|line| {
        let w: Vec<&str> = line.split_whitespace().collect();
        (w.first() == Some(&mac) && !w.contains(&"permanent")).then(|| ())?;
        w.iter()
            .position(|t| *t == "dev")
            .and_then(|i| w.get(i + 1))
            .map(|p| p.to_string())
    })
}

/// The trunk port an access point's handshakes arrive on.
async fn port_for_endpoint(endpoint: &str) -> Option<String> {
    let (ip, Some(scope)) = endpoint_host(endpoint)? else {
        return None;
    };
    let bridge = scope.rsplit_once('.').map(|(b, _)| b)?;
    let neigh = command_output("ip", &["-6", "neigh", "show", "dev", scope]).await?;
    let mac = neighbour_mac(&neigh, ip)?;
    let vlan = crate::backhaul::BACKHAUL_VLAN.to_string();
    let fdb = command_output("bridge", &["fdb", "show", "br", bridge, "vlan", &vlan]).await?;
    fdb_port(&fdb, &mac)
}

async fn repin_endpoint(interface: &str, public_key: &str, endpoint: &str) {
    if command_output(
        "wg",
        &["set", interface, "peer", public_key, "endpoint", endpoint],
    )
    .await
    .is_none()
    {
        tracing::debug!("liveness: could not re-pin {endpoint} on {interface}");
    }
}

async fn run_core() {
    let mut monitors: BTreeMap<String, Monitor> = BTreeMap::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_persist = Instant::now();
    let mut reach_due = Instant::now();
    let mut applied: Option<BTreeSet<String>> = None;
    loop {
        tick.tick().await;
        let now = Instant::now();
        let now_unix = unix_now();
        let _guard = satellite::REGISTRY_LOCK.lock().await;
        let mut registry = satellite::load_registry();
        monitors.retain(|name, _| registry.iter().any(|s| s.name == *name));
        let mut changed = false;
        let mut live_ports = BTreeSet::new();
        for sat in &mut registry {
            let interface = crate::mgmt_tunnel::core_interface_name(&sat.name);
            probe(&sat.mgmt_address);
            let obs = observe_interface(&interface).await;
            let live =
                monitors
                    .entry(sat.name.clone())
                    .or_default()
                    .observe(obs.as_ref(), now, now_unix);
            match obs.as_ref().and_then(|o| o.endpoint.as_deref()) {
                Some(ep) if sat.endpoint.as_deref() != Some(ep) => {
                    sat.endpoint = Some(ep.to_string());
                    changed = true;
                }
                Some(_) => {}
                None => {
                    if let (Some(ep), true) = (&sat.endpoint, obs.is_some()) {
                        repin_endpoint(&interface, &sat.public_key, ep).await;
                    }
                }
            }
            let port = if live {
                match &sat.endpoint {
                    Some(ep) => port_for_endpoint(ep).await,
                    None => None,
                }
            } else {
                None
            };
            if let Some(p) = &port {
                live_ports.insert(p.clone());
            }
            if sat.port != port {
                sat.port = port;
                changed = true;
            }
            if sat.live != live {
                sat.live = live;
                changed = true;
                crate::activity::log(
                    "satellite",
                    if live { "up" } else { "dark" },
                    live,
                    &format!(
                        "Satellite '{}' is {}",
                        sat.name,
                        if live {
                            "in contact; its trunk port is open"
                        } else {
                            "out of contact; its trunk port is closed"
                        }
                    ),
                    None,
                );
            }
            if live {
                sat.last_seen = now_unix as i64;
            }
        }
        if changed || (registry.iter().any(|s| s.live) && last_persist.elapsed() > PERSIST_EVERY) {
            if let Err(e) = satellite::persist_registry(&registry).await {
                tracing::error!("liveness: registry not saved: {e}");
            }
            last_persist = Instant::now();
        }
        drop(_guard);
        if crate::bridge_filters::gate_installed()
            && (!live_ports.is_empty() || applied.as_ref() != Some(&live_ports))
        {
            match crate::bridge_filters::set_live_ports(&live_ports).await {
                Ok(()) => applied = Some(live_ports),
                Err(e) => tracing::debug!("liveness: live set not applied: {e}"),
            }
        }
        if !registry.is_empty() && now >= reach_due {
            if let Err(e) = crate::update_reach::refresh().await {
                tracing::warn!("liveness: update reach not refreshed: {e}");
            }
            reach_due = now + crate::update_reach::REFRESH;
        }
    }
}

// ── Access point ────────────────────────────────────────────────────────────

const STATE_UNKNOWN: u8 = 0;
const STATE_UP: u8 = 1;
const STATE_DARK: u8 = 2;
static AP_STATE: AtomicU8 = AtomicU8::new(STATE_UNKNOWN);
static TUNNEL_LIVE: AtomicU8 = AtomicU8::new(0);

/// What an access point's status reports: whether its tunnel is live, and
/// whether it is dark (`None` before the first reading).
pub fn ap_status() -> (bool, Option<bool>) {
    let dark = match AP_STATE.load(Ordering::Relaxed) {
        STATE_UP => Some(false),
        STATE_DARK => Some(true),
        _ => None,
    };
    (TUNNEL_LIVE.load(Ordering::Relaxed) == 1, dark)
}

/// Bridge ports that are neither the uplink nor a radio.
fn access_ports(uplink: &str) -> Vec<String> {
    let bridge = crate::ethernet::DEFAULT_LAN_BRIDGE;
    std::fs::read_dir(format!("/sys/class/net/{bridge}/brif"))
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n != uplink && !n.starts_with("wlan") && !n.starts_with("phy"))
                .collect()
        })
        .unwrap_or_default()
}

async fn set_ports(ports: &[String], state: &str) {
    for port in ports {
        let _ = crate::run_quiet_async(
            tokio::process::Command::new("ip").args(["link", "set", "dev", port, state]),
        )
        .await;
    }
}

async fn go_dark(uplink: &str) {
    tracing::warn!("liveness: the Core is unreachable; SSIDs down, access ports cut");
    let _ = crate::run_quiet_async(tokio::process::Command::new("wifi").arg("down")).await;
    set_ports(&access_ports(uplink), "down").await;
    crate::activity::log(
        "satellite",
        "dark",
        false,
        "Lost the Core: Wi-Fi off and access ports cut until it is back",
        None,
    );
}

async fn resume(uplink: &str) {
    tracing::info!("liveness: the Core is back; SSIDs up, access ports restored");
    set_ports(&access_ports(uplink), "up").await;
    let _ = crate::run_quiet_async(tokio::process::Command::new("wifi").arg("up")).await;
    crate::activity::log(
        "satellite",
        "resumed",
        true,
        "The Core is back: Wi-Fi on and access ports restored",
        None,
    );
}

async fn run_ap() {
    let role = satellite::load_role();
    let uplink = role.uplink.unwrap_or_default();
    let core_address = role.core_address.unwrap_or_default();
    let mut monitor = Monitor::default();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut live_since: Option<Instant> = None;
    loop {
        tick.tick().await;
        let now = Instant::now();
        if !core_address.is_empty() {
            probe(&core_address);
        }
        let obs = observe_interface(crate::mgmt_tunnel::AP_INTERFACE).await;
        let live = monitor.observe(obs.as_ref(), now, unix_now());
        TUNNEL_LIVE.store(live as u8, Ordering::Relaxed);
        match AP_STATE.load(Ordering::Relaxed) {
            STATE_UP if !live => {
                go_dark(&uplink).await;
                AP_STATE.store(STATE_DARK, Ordering::Relaxed);
            }
            STATE_UP => {}
            STATE_DARK if live => {
                let since = *live_since.get_or_insert(now);
                if now.duration_since(since) >= RESUME_AFTER {
                    resume(&uplink).await;
                    AP_STATE.store(STATE_UP, Ordering::Relaxed);
                    live_since = None;
                }
            }
            STATE_DARK => live_since = None,
            _ if live => AP_STATE.store(STATE_UP, Ordering::Relaxed),
            _ => {
                go_dark(&uplink).await;
                AP_STATE.store(STATE_DARK, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "PRIV=\tPUB=\t51900\toff\n\
        APKEY=\tPSK=\t[fe80::1%br-lan.4094]:51899\t10.42.1.2/32\t1700000000\t1234\t5678\t25\n";

    #[test]
    fn dump_yields_the_peer() {
        let o = parse_dump(DUMP).unwrap();
        assert_eq!(o.endpoint.as_deref(), Some("[fe80::1%br-lan.4094]:51899"));
        assert_eq!((o.latest_handshake, o.rx_bytes), (1700000000, 1234));
        let none =
            parse_dump("PRIV=\tPUB=\t51900\toff\nK=\tP=\t(none)\t10.42.1.2/32\t0\t0\t0\t25\n")
                .unwrap();
        assert_eq!(none.endpoint, None);
        assert!(parse_dump("PRIV=\tPUB=\t51900\toff\n").is_none());
    }

    #[test]
    fn traffic_keeps_a_tunnel_live_and_silence_darkens_it() {
        let t0 = Instant::now();
        let mut m = Monitor::default();
        let mut o = Observation {
            endpoint: None,
            latest_handshake: 1_000_000,
            rx_bytes: 10,
        };
        // First sight with a fresh handshake counts.
        assert!(m.observe(Some(&o), t0, 1_000_050));
        // Silence past the threshold is dark.
        assert!(!m.observe(Some(&o), t0 + THRESHOLD, 1_000_100));
        // Received bytes bring it back.
        o.rx_bytes = 11;
        assert!(m.observe(Some(&o), t0 + THRESHOLD + TICK, 1_000_105));
        // A counter reset after an interface restart is still progress.
        o.rx_bytes = 1;
        assert!(m.observe(Some(&o), t0 + THRESHOLD + 2 * TICK, 1_000_110));
        // No interface at all is dead.
        assert!(!m.observe(None, t0 + THRESHOLD + 3 * TICK, 1_000_115));
    }

    #[test]
    fn a_stale_handshake_on_first_sight_is_not_live() {
        let mut m = Monitor::default();
        let o = Observation {
            endpoint: None,
            latest_handshake: 1_000_000,
            rx_bytes: 99,
        };
        assert!(!m.observe(Some(&o), Instant::now(), 1_000_000 + HANDSHAKE_FRESH));
        let never = Observation::default();
        assert!(!Monitor::default().observe(Some(&never), Instant::now(), 5));
    }

    #[test]
    fn endpoint_neighbour_and_fdb_resolve_to_a_port() {
        assert_eq!(
            endpoint_host("[fe80::1%br-lan.4094]:51899"),
            Some(("fe80::1", Some("br-lan.4094")))
        );
        assert_eq!(endpoint_host("10.0.0.2:51899"), Some(("10.0.0.2", None)));
        let neigh = "fe80::2 FAILED\nfe80::1 lladdr fe:fe:fe:82:5d:08 router STALE\n";
        assert_eq!(
            neighbour_mac(neigh, "fe80::1").as_deref(),
            Some("fe:fe:fe:82:5d:08")
        );
        assert_eq!(neighbour_mac(neigh, "fe80::2"), None);
        let fdb = "fe:fe:fe:82:5d:08 dev eth0 vlan 4094 master br-lan \n\
                   18:66:96:a2:62:8c dev br-lan vlan 4094 master br-lan permanent\n";
        assert_eq!(fdb_port(fdb, "fe:fe:fe:82:5d:08").as_deref(), Some("eth0"));
        assert_eq!(fdb_port(fdb, "18:66:96:a2:62:8c"), None);
    }
}

//! The Core's RPC path to an access point, inside the management tunnel
//! (design D4, D5, D25).
//!
//! The access point serves a small JSON-RPC API on its tunnel address: the
//! Core pushes the sync payload, reads status and inventory, and releases the
//! access point on unpair. The access point trusts a request by its source
//! address alone: inside the tunnel, WireGuard's cryptokey routing binds that
//! address to the Core's key, and the access point's firewall admits the port
//! from the tunnel zone only.

use std::future::ready;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use clap::Parser;
use rpc_toolkit::{from_fn_async, from_fn_async_local, HandlerExt, ParentHandler, Server};
use serde::{Deserialize, Serialize};
use uciedit::{dump_all, parse_all, Arena};

use crate::prelude::*;
use crate::satellite::{
    self, Inventory, PortInfo, RadioInfo, RouterRole, SyncSnapshot, TunnelStatus,
};
use crate::utils::DeserializeStdin;
use crate::ServerContext;

pub const PORT: u16 = 8720;
const PATH: &str = "/rpc";
const TIMEOUT: Duration = Duration::from_secs(20);

static RUNNING: Mutex<bool> = Mutex::new(false);
/// The Core's current generation, as its last status call said it.
static CORE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// The generation the Core last announced; 0 before any contact.
pub fn core_generation() -> u64 {
    CORE_GENERATION.load(Ordering::Relaxed)
}

fn core_address() -> Option<IpAddr> {
    satellite::load_role()
        .core_address
        .and_then(|a| a.parse().ok())
}

// ── Access point: server ────────────────────────────────────────────────────

fn tunnel_api() -> ParentHandler<ServerContext> {
    ParentHandler::new()
        .subcommand("apply", from_fn_async_local(apply).no_display())
        .subcommand("status", from_fn_async(status_handler).no_display())
        .subcommand("release", from_fn_async_local(release_handler).no_display())
}

/// The Core's payload; an older generation is refused, the applied one is a no-op.
#[instrument(skip_all)]
async fn apply(
    _ctx: ServerContext,
    DeserializeStdin(snapshot): DeserializeStdin<SyncSnapshot>,
) -> Result<(), Error> {
    crate::ap::apply_snapshot(&snapshot).await
}

#[derive(Debug, Default, Serialize, Deserialize, Parser, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
pub struct StatusParams {
    /// The Core's current generation, which the access point must have
    /// applied before it serves clients again (D18).
    #[serde(default)]
    #[clap(long, default_value_t = 0)]
    pub core_generation: u64,
}
rpc_toolkit::reflect_ts!(StatusParams);

#[instrument(skip_all)]
async fn status_handler(_ctx: ServerContext, p: StatusParams) -> Result<TunnelStatus, Error> {
    if p.core_generation > 0 {
        CORE_GENERATION.store(p.core_generation, Ordering::Relaxed);
    }
    let role = satellite::load_role();
    let (tunnel_live, dark) = crate::liveness::ap_status();
    Ok(TunnelStatus {
        generation: role.generation,
        tunnel_live,
        dark,
        inventory: inventory(role.uplink.as_deref().unwrap_or("")),
    })
}

/// Answers first; the release tears the tunnel down a moment later.
#[instrument(skip_all)]
async fn release_handler(_ctx: ServerContext) -> Result<(), Error> {
    std::thread::spawn(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("release runtime")
            .block_on(async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                match release_self().await {
                    Ok(()) => {
                        tracing::info!("control: released by the Core; waiting to be adopted")
                    }
                    Err(e) => tracing::error!("control: release failed: {e}"),
                }
            });
    });
    Ok(())
}

/// Listens on an adopted access point. Idempotent across re-adoptions.
pub fn start() {
    let role = satellite::load_role();
    if role.role != RouterRole::Satellite || role.core_key.is_none() {
        return;
    }
    {
        let mut running = RUNNING.lock().unwrap();
        if *running {
            return;
        }
        *running = true;
    }
    // Apply and release rewrite UCI and hold a uciedit `Arena` across awaits.
    if let Err(e) = std::thread::Builder::new()
        .name("control".into())
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("control runtime")
                .block_on(async {
                    if let Err(e) = serve().await {
                        tracing::error!("control: tunnel API failed: {e}");
                    }
                });
            *RUNNING.lock().unwrap() = false;
        })
    {
        tracing::error!("control: failed to start thread: {e}");
        *RUNNING.lock().unwrap() = false;
    }
}

async fn serve() -> Result<(), Error> {
    let ctx = ServerContext::default();
    let server = Server::new(move || ready(Ok(ctx.clone())), tunnel_api()).for_http();
    let app = Router::new()
        .route(PATH, post(server))
        .layer(axum::middleware::from_fn(only_the_core));
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], PORT)))
        .await
        .with_kind(ErrorKind::Network)?;
    tracing::info!("control: tunnel API listening on port {PORT}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .with_kind(ErrorKind::Network)
}

async fn only_the_core(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if Some(peer.ip()) != core_address() {
        tracing::debug!("control: refused a request from {}", peer.ip());
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    next.run(req).await
}

/// Back to a candidate: the tunnel goes, the base layout stays, and the
/// candidate listener answers the next Core on the backhaul.
async fn release_self() -> Result<(), Error> {
    {
        let arena = Arena::new();
        let mut cfgs = parse_all("/etc/config", &arena, &["network", "firewall", "system"]).await?;
        crate::mgmt_tunnel::remove_ap_end(&mut cfgs)?;
        dump_all("/etc/config", cfgs).await?;
    }
    let mut role = satellite::load_role();
    role.core_key = None;
    role.core_endpoint = None;
    role.core_address = None;
    role.generation = 0;
    satellite::persist_role(&role).await?;
    CORE_GENERATION.store(0, Ordering::Relaxed);
    for (service, action) in [
        ("network", "reload"),
        ("firewall", "reload"),
        ("sysntpd", "restart"),
    ] {
        let _ = crate::run_quiet_async(
            tokio::process::Command::new(format!("/etc/init.d/{service}")).arg(action),
        )
        .await;
    }
    crate::activity::log(
        "satellite",
        "released",
        true,
        "Released by the Core; waiting to be adopted again",
        None,
    );
    crate::adoption::start();
    Ok(())
}

// ── Access point: inventory ─────────────────────────────────────────────────

/// `2g`, `5g` or `6g` for an `iw phy` band header.
pub fn band_name(header: &str) -> Option<&'static str> {
    match header.trim().trim_end_matches(':') {
        "Band 1" => Some("2g"),
        "Band 2" => Some("5g"),
        "Band 4" => Some("6g"),
        _ => None,
    }
}

/// The bands `iw phy <phy> info` lists.
pub fn parse_bands(info: &str) -> Vec<String> {
    info.lines()
        .filter_map(|l| band_name(l).map(str::to_string))
        .collect()
}

/// The phy a wireless-config `path` names: the base path's n-th phy, where
/// `+n` selects among several on one device.
pub fn phy_for_path(path: &str, phys_at: &dyn Fn(&str) -> Vec<String>) -> Option<String> {
    let (base, index) = match path.rsplit_once('+') {
        Some((b, i)) => (b, i.parse::<usize>().ok()?),
        None => (path, 0),
    };
    let mut phys = phys_at(base);
    phys.sort();
    phys.get(index).cloned()
}

fn phys_under(base: &str) -> Vec<String> {
    std::fs::read_dir(format!("/sys/devices/platform/{base}/ieee80211"))
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn radios() -> Vec<RadioInfo> {
    let out = std::process::Command::new("uci")
        .args(["-q", "show", "wireless"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let mut radios = Vec::new();
    for line in out.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(name) = key
            .strip_prefix("wireless.")
            .and_then(|k| k.strip_suffix(".path"))
        else {
            continue;
        };
        let path = value.trim_matches('\'');
        let Some(phy) = phy_for_path(path, &phys_under) else {
            continue;
        };
        let info = std::process::Command::new("iw")
            .args(["phy", &phy, "info"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        radios.push(RadioInfo {
            name: name.to_string(),
            phy,
            bands: parse_bands(&info),
        });
    }
    radios
}

fn ports(uplink: &str) -> Vec<PortInfo> {
    let mut ports: Vec<PortInfo> = std::fs::read_dir("/sys/class/net")
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with("eth") || n.starts_with("lan") || n.starts_with("wan"))
                .filter(|n| !n.contains('.') && !n.contains('@'))
                .map(|n| PortInfo {
                    link: std::fs::read_to_string(format!("/sys/class/net/{n}/carrier"))
                        .is_ok_and(|c| c.trim() == "1"),
                    uplink: n == uplink,
                    name: n,
                })
                .collect()
        })
        .unwrap_or_default();
    ports.sort_by(|a, b| a.name.cmp(&b.name));
    ports
}

fn inventory(uplink: &str) -> Inventory {
    Inventory {
        ports: ports(uplink),
        radios: radios(),
    }
}

// ── Core: client ────────────────────────────────────────────────────────────

async fn call<T: serde::de::DeserializeOwned>(
    addr: IpAddr,
    method: &str,
    params: serde_json::Value,
) -> Result<T, Error> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| Error::other(format!("tunnel client: {e}")))?;
    let url = format!("http://{}{PATH}", SocketAddr::new(addr, PORT));
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 0, "method": method, "params": params });
    let res = client
        .post(url)
        .header(http::header::CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::new(eyre!("{method} at {addr}: {e}"), ErrorKind::Network))?;
    let status = res.status();
    let value: serde_json::Value = res.json().await.map_err(|e| {
        Error::new(
            eyre!("{method} at {addr}: {status}: {e}"),
            ErrorKind::Network,
        )
    })?;
    if let Some(err) = value.get("error") {
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        let data = err
            .get("data")
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        return Err(Error::new(
            eyre!("{method} at {addr}: {message}{data}"),
            ErrorKind::Network,
        ));
    }
    serde_json::from_value(
        value
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map_err(|e| {
        Error::new(
            eyre!("{method} at {addr}: bad result: {e}"),
            ErrorKind::Network,
        )
    })
}

pub async fn push(addr: IpAddr, snapshot: &SyncSnapshot) -> Result<(), Error> {
    call::<serde_json::Value>(addr, "apply", serde_json::to_value(snapshot).unwrap()).await?;
    Ok(())
}

pub async fn status(addr: IpAddr, core_generation: u64) -> Result<TunnelStatus, Error> {
    call(
        addr,
        "status",
        serde_json::json!({ "coreGeneration": core_generation }),
    )
    .await
}

/// Tells the access point at `addr` that the Core has forgotten it. `Ok(false)`
/// when it did not answer: a dark access point learns the same from its
/// tunnel never coming back.
pub async fn release(addr: IpAddr) -> Result<bool, Error> {
    match tokio::time::timeout(
        Duration::from_secs(3),
        call::<serde_json::Value>(addr, "release", serde_json::json!({})),
    )
    .await
    {
        Ok(Ok(_)) => Ok(true),
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_come_from_iw_headers() {
        let info = "Wiphy phy0\n\twiphy index: 0\n\tBand 1:\n\t\tCapabilities\n\tBand 2:\n\t\tHE Iftypes\n";
        assert_eq!(parse_bands(info), vec!["2g", "5g"]);
        assert_eq!(band_name("\tBand 4:"), Some("6g"));
        assert_eq!(band_name("Band 3:"), None);
    }

    #[test]
    fn a_path_selects_the_nth_phy_on_its_device() {
        let phys = |base: &str| -> Vec<String> {
            if base == "soc/pcie/0002:01:00.0" {
                vec!["phy1".into(), "phy0".into()]
            } else {
                Vec::new()
            }
        };
        assert_eq!(
            phy_for_path("soc/pcie/0002:01:00.0", &phys).as_deref(),
            Some("phy0")
        );
        assert_eq!(
            phy_for_path("soc/pcie/0002:01:00.0+1", &phys).as_deref(),
            Some("phy1")
        );
        assert_eq!(phy_for_path("soc/pcie/0002:01:00.0+2", &phys), None);
        assert_eq!(phy_for_path("soc/other", &phys), None);
    }

    #[tokio::test]
    async fn release_reports_silence_as_false() {
        let t = std::time::Instant::now();
        let r = tokio::time::timeout(
            Duration::from_secs(6),
            release("127.0.0.1".parse().unwrap()),
        )
        .await
        .unwrap();
        assert!(matches!(r, Ok(false)), "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(5));
    }
}

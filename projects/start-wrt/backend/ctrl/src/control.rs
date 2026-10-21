//! The main router's RPC path to an access point, inside the management tunnel
//! (design D4, D5, D25).
//!
//! The access point serves a small JSON-RPC API on its tunnel address: the
//! main router pushes the sync payload, reads status and inventory, orders and
//! sends a firmware image, and releases the access point on unpair. The access
//! point trusts a request by its source address alone: inside the tunnel,
//! WireGuard's cryptokey routing binds that address to the main router's key,
//! and the access point's firewall admits the port from the tunnel zone only.

use std::future::ready;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::response::IntoResponse;
use axum::routing::{post, put};
use axum::Router;
use clap::Parser;
use rpc_toolkit::{from_fn_async, from_fn_async_local, HandlerExt, ParentHandler, Server};
use serde::{Deserialize, Serialize};
use uciedit::{dump_all, parse_all, Arena};

use crate::access_points::{
    self, ApUpdate, ApUpdateState, ClientInfo, Inventory, PortInfo, RadioInfo, RouterRole,
    SyncSnapshot, TunnelStatus,
};
use crate::prelude::*;
use crate::utils::DeserializeStdin;
use crate::ServerContext;

pub const PORT: u16 = 8720;
const PATH: &str = "/rpc";
const FIRMWARE_PATH: &str = "/firmware";
const TIMEOUT: Duration = Duration::from_secs(20);
/// Sending an image over the tunnel.
const FIRMWARE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const FIRMWARE_FILE: &str = "/tmp/firmware.bin";

static RUNNING: Mutex<bool> = Mutex::new(false);
/// The main router's current generation, as its last status call said it.
static MAIN_GENERATION: AtomicU64 = AtomicU64::new(0);
/// The main router's firmware version, as its last status call said it.
static MAIN_VERSION: Mutex<String> = Mutex::new(String::new());
/// This access point's own firmware update, once the main router has ordered
/// one.
static AP_UPDATE: Mutex<Option<ApUpdate>> = Mutex::new(None);
/// The image the main router ordered, until it arrives.
static ORDER: Mutex<Option<FirmwareOrder>> = Mutex::new(None);

/// The generation the main router last announced; 0 before any contact.
pub fn main_generation() -> u64 {
    MAIN_GENERATION.load(Ordering::Relaxed)
}

/// The main router's firmware version; empty before any contact.
pub fn main_version() -> String {
    MAIN_VERSION.lock().unwrap().clone()
}

fn main_address() -> Option<IpAddr> {
    access_points::load_role()
        .main_address
        .and_then(|a| a.parse().ok())
}

// ── Access point: server ────────────────────────────────────────────────────

fn tunnel_api() -> ParentHandler<ServerContext> {
    ParentHandler::new()
        .subcommand("apply", from_fn_async_local(apply).no_display())
        .subcommand("status", from_fn_async(status_handler).no_display())
        .subcommand("release", from_fn_async_local(release_handler).no_display())
        .subcommand("update", from_fn_async(update_handler).no_display())
}

/// The main router's payload; an older generation is refused, the applied one
/// is a no-op.
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
    /// The main router's current generation, which the access point must have
    /// applied before it serves clients again (D18).
    #[serde(default)]
    #[clap(long, default_value_t = 0)]
    pub main_generation: u64,
    /// The main router's firmware version, which the access point shows against
    /// its own.
    #[serde(default)]
    #[clap(long, default_value = "")]
    pub main_version: String,
    /// The name the main router knows this access point by; an access point
    /// adopted before names were recorded takes it from here.
    #[serde(default)]
    #[clap(long, default_value = "")]
    pub name: String,
    /// The main router's payload format; one newer than this firmware reads
    /// darkens the access point until it is updated (D17).
    #[serde(default)]
    #[clap(long, default_value_t = 0)]
    pub main_format: u32,
}
rpc_toolkit::reflect_ts!(StatusParams);

#[instrument(skip_all)]
async fn status_handler(_ctx: ServerContext, p: StatusParams) -> Result<TunnelStatus, Error> {
    if p.main_generation > 0 {
        MAIN_GENERATION.store(p.main_generation, Ordering::Relaxed);
    }
    if !p.main_version.is_empty() {
        *MAIN_VERSION.lock().unwrap() = p.main_version;
    }
    if p.main_format > 0 {
        crate::ap::check_format(p.main_format);
    }
    let mut role = access_points::load_role();
    if role.name.is_none() && !p.name.is_empty() {
        role.name = Some(p.name);
        let _ = access_points::persist_role(&role).await;
    }
    let (tunnel_live, dark) = crate::liveness::ap_status();
    let uplink = role.uplink.unwrap_or_default();
    Ok(TunnelStatus {
        generation: role.generation,
        tunnel_live,
        dark,
        inventory: inventory(&uplink),
        clients: clients(&uplink).await,
        version: env!("CARGO_PKG_VERSION").to_string(),
        recovery_ssid: crate::recovery::wifi_available().then(|| crate::recovery::ssid(&uplink)),
        format: access_points::SYNC_FORMAT,
        needs_update: crate::ap::needs_update(),
        update: AP_UPDATE.lock().unwrap().clone(),
        loop_blocked: crate::loop_guard::blocked(),
        platform: crate::registry::device_info::detect_platform(),
    })
}

/// The main router's firmware order: the version, and the image's signed
/// metadata from the main router's update source.
#[derive(Clone, Debug, Serialize, Deserialize, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareOrder {
    pub target_version: String,
    pub asset: crate::update::Asset,
}
rpc_toolkit::reflect_ts!(FirmwareOrder);

/// Takes the order once its signatures check; the image follows on
/// `PUT /firmware`. Status reports the outcome until the reboot.
#[instrument(skip_all)]
async fn update_handler(
    _ctx: ServerContext,
    DeserializeStdin(order): DeserializeStdin<FirmwareOrder>,
) -> Result<(), Error> {
    order
        .asset
        .validate(crate::registry::os::SIG_CONTEXT, order.asset.all_signers())?;
    let mut current = AP_UPDATE.lock().unwrap();
    let mut pending = ORDER.lock().unwrap();
    // An order whose image never arrived is replaced.
    if pending.is_none()
        && current
            .as_ref()
            .is_some_and(|u| u.state == ApUpdateState::Updating)
    {
        return Err(Error::new(
            eyre!("an update is already running"),
            ErrorKind::InvalidRequest,
        ));
    }
    *current = Some(ApUpdate {
        target: order.target_version.clone(),
        state: ApUpdateState::Updating,
        message: None,
    });
    *pending = Some(order);
    Ok(())
}

/// The ordered image: written to `/tmp`, checked against the order's
/// commitment, then flashed after the reply.
async fn firmware_handler(body: axum::body::Body) -> axum::response::Response {
    use axum::http::StatusCode;

    let Some(order) = ORDER.lock().unwrap().take() else {
        return (StatusCode::CONFLICT, "no update ordered").into_response();
    };
    if let Err(e) = receive_firmware(body, &order.asset).await {
        let message = format!("{:#}", e.source);
        update_failed(&order.target_version, &message);
        return (StatusCode::BAD_REQUEST, message).into_response();
    }
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("update runtime")
            .block_on(async {
                let path = std::path::Path::new(FIRMWARE_FILE);
                if let Err(e) = crate::update::flash(path, &order.target_version).await {
                    update_failed(&order.target_version, &format!("{:#}", e.source));
                }
            });
    });
    StatusCode::NO_CONTENT.into_response()
}

async fn receive_firmware(
    body: axum::body::Body,
    asset: &crate::update::Asset,
) -> Result<(), Error> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let path = std::path::Path::new(FIRMWARE_FILE);
    let mut file = tokio::fs::File::create(path)
        .await
        .with_kind(ErrorKind::Filesystem)?;
    let mut stream = body.into_data_stream();
    let mut size = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| Error::new(eyre!("receiving the image: {e}"), ErrorKind::Network))?;
        size += chunk.len() as u64;
        if size > asset.commitment.size {
            return Err(Error::new(
                eyre!("the image is larger than ordered"),
                ErrorKind::InvalidRequest,
            ));
        }
        file.write_all(&chunk)
            .await
            .with_kind(ErrorKind::Filesystem)?;
    }
    file.flush().await.with_kind(ErrorKind::Filesystem)?;
    drop(file);
    asset.commitment.verify_file(path).await
}

fn update_failed(target: &str, message: &str) {
    tracing::error!("control: update to {target} failed: {message}");
    crate::activity::log(
        "system",
        "update",
        false,
        &format!("Update to v{target} failed"),
        Some(message),
    );
    *AP_UPDATE.lock().unwrap() = Some(ApUpdate {
        target: target.to_string(),
        state: ApUpdateState::Failed,
        message: Some(message.to_string()),
    });
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
                        tracing::info!(
                            "control: released by the main router; waiting to be adopted"
                        )
                    }
                    Err(e) => tracing::error!("control: release failed: {e}"),
                }
            });
    });
    Ok(())
}

/// Listens on an adopted access point. Idempotent across re-adoptions.
pub fn start() {
    let role = access_points::load_role();
    if role.role != RouterRole::AccessPoint || role.main_key.is_none() {
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
        .route(FIRMWARE_PATH, put(firmware_handler))
        .layer(axum::middleware::from_fn(only_the_main_router));
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

async fn only_the_main_router(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if Some(peer.ip()) != main_address() {
        tracing::debug!("control: refused a request from {}", peer.ip());
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    next.run(req).await
}

/// Back to a candidate: the tunnel goes, the base layout stays, and the
/// candidate listener answers the next main router on the backhaul.
async fn release_self() -> Result<(), Error> {
    {
        let arena = Arena::new();
        let mut cfgs = parse_all("/etc/config", &arena, &["network", "firewall", "system"]).await?;
        crate::mgmt_tunnel::remove_ap_end(&mut cfgs)?;
        dump_all("/etc/config", cfgs).await?;
    }
    let mut role = access_points::load_role();
    role.main_key = None;
    role.main_endpoint = None;
    role.main_address = None;
    role.generation = 0;
    access_points::persist_role(&role).await?;
    MAIN_GENERATION.store(0, Ordering::Relaxed);
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
        "access-point",
        "released",
        true,
        "Released by the main router; waiting to be adopted again",
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

pub fn inventory(uplink: &str) -> Inventory {
    Inventory {
        ports: ports(uplink),
        radios: radios(),
    }
}

/// `bridge fdb show br <bridge>`: learned entries as (MAC, port, VLAN).
pub fn fdb_entries(output: &str) -> Vec<(String, String, Option<u16>)> {
    output
        .lines()
        .filter(|l| l.contains("master") && !l.contains("permanent"))
        .filter_map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            let mac = w.first()?.to_uppercase();
            let port = w
                .iter()
                .position(|t| *t == "dev")
                .and_then(|i| w.get(i + 1))?;
            let vlan = w
                .iter()
                .position(|t| *t == "vlan")
                .and_then(|i| w.get(i + 1))
                .and_then(|v| v.parse().ok());
            Some((mac, port.to_string(), vlan))
        })
        .collect()
}

async fn command(program: &str, args: &[&str]) -> String {
    tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Wi-Fi band label per `uci show wireless` section: radios by their band,
/// interfaces by their radio's.
pub fn wifi_band_labels(uci: &str) -> std::collections::HashMap<String, String> {
    let mut radio_band: std::collections::HashMap<&str, &str> = Default::default();
    let mut iface_radio: Vec<(&str, &str)> = Vec::new();
    for line in uci.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim_matches('\'');
        let mut parts = key.splitn(3, '.');
        let (Some(_), Some(section), Some(option)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        match option {
            "band" => {
                radio_band.insert(section, value);
            }
            "device" => iface_radio.push((section, value)),
            _ => {}
        }
    }
    let label = |band: &str| match band {
        "2g" => "Wi-Fi 2.4GHz",
        "5g" => "Wi-Fi 5GHz",
        "6g" => "Wi-Fi 6GHz",
        _ => "Wi-Fi",
    };
    iface_radio
        .into_iter()
        .map(|(iface, radio)| {
            (
                iface.to_string(),
                label(radio_band.get(radio).copied().unwrap_or("")).to_string(),
            )
        })
        .collect()
}

/// Wi-Fi clients by MAC with their band label, from hostapd.
async fn wifi_clients() -> std::collections::HashMap<String, String> {
    let labels = wifi_band_labels(&command("uci", &["-q", "show", "wireless"]).await);
    let mut out = std::collections::HashMap::new();
    for hostapd in command("ubus", &["list"]).await.lines() {
        let Some(iface) = hostapd.strip_prefix("hostapd.") else {
            continue;
        };
        let band = labels
            .get(iface)
            .cloned()
            .unwrap_or_else(|| "Wi-Fi".to_string());
        let json = command("ubus", &["call", hostapd, "get_clients"]).await;
        if let Some(clients) = serde_json::from_str::<serde_json::Value>(&json)
            .ok()
            .and_then(|v| v.get("clients").and_then(|c| c.as_object()).cloned())
        {
            for mac in clients.keys() {
                out.insert(mac.to_uppercase(), band.clone());
            }
        }
    }
    out
}

/// The devices behind this access point: every MAC the bridge learned on a
/// port other than the uplink, by Wi-Fi band when hostapd knows it, else by
/// port.
pub async fn clients(uplink: &str) -> Vec<ClientInfo> {
    let wifi = wifi_clients().await;
    let fdb = tokio::process::Command::new("bridge")
        .args(["fdb", "show", "br", crate::ethernet::DEFAULT_LAN_BRIDGE])
        .output()
        .await
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let mut seen = std::collections::BTreeSet::new();
    fdb_entries(&fdb)
        .into_iter()
        .filter(|(_, port, _)| port != uplink && !port.starts_with("br-"))
        .filter(|(mac, _, _)| seen.insert(mac.clone()))
        .map(|(mac, port, vlan)| ClientInfo {
            via: wifi.get(&mac).cloned().unwrap_or(port),
            mac,
            vlan,
        })
        .collect()
}

// ── Main router: client ─────────────────────────────────────────────────────

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

pub async fn status(addr: IpAddr, name: &str, main_generation: u64) -> Result<TunnelStatus, Error> {
    call(
        addr,
        "status",
        serde_json::json!({
            "mainGeneration": main_generation,
            "mainVersion": env!("CARGO_PKG_VERSION"),
            "name": name,
            "mainFormat": access_points::SYNC_FORMAT,
        }),
    )
    .await
}

/// Orders the access point at `addr` to update to `target` from `registry`.
/// Orders the access point at `addr` to take `target`'s image.
pub async fn update(addr: IpAddr, target: &str, asset: &crate::update::Asset) -> Result<(), Error> {
    call::<serde_json::Value>(
        addr,
        "update",
        serde_json::json!({ "targetVersion": target, "asset": asset }),
    )
    .await?;
    Ok(())
}

/// Sends the ordered image; `Ok` once the access point has checked it.
pub async fn send_firmware(addr: IpAddr, image: &std::path::Path) -> Result<(), Error> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(FIRMWARE_TIMEOUT)
        .build()
        .map_err(|e| Error::other(format!("tunnel client: {e}")))?;
    let file = tokio::fs::File::open(image)
        .await
        .with_kind(ErrorKind::Filesystem)?;
    let len = file
        .metadata()
        .await
        .with_kind(ErrorKind::Filesystem)?
        .len();
    let res = client
        .put(format!(
            "http://{}{FIRMWARE_PATH}",
            SocketAddr::new(addr, PORT)
        ))
        .header(http::header::CONTENT_LENGTH, len)
        .body(reqwest::Body::wrap_stream(
            tokio_util::io::ReaderStream::new(file),
        ))
        .send()
        .await
        .map_err(|e| {
            Error::new(
                eyre!("sending the image to {addr}: {e}"),
                ErrorKind::Network,
            )
        })?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(Error::new(
            eyre!("{addr} refused the image: {status}: {text}"),
            ErrorKind::Network,
        ));
    }
    Ok(())
}

/// Tells the access point at `addr` that the main router has forgotten it.
/// `Ok(false)` when it did not answer: a dark access point learns the same from
/// its tunnel never coming back.
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

    #[test]
    fn band_labels_follow_the_interface_to_its_radio() {
        let uci = "wireless.radio0=wifi-device\nwireless.radio0.band='2g'\n\
                   wireless.default_radio0=wifi-iface\nwireless.default_radio0.device='radio0'\n\
                   wireless.radio1.band='5g'\nwireless.default_radio1.device='radio1'\n";
        let l = wifi_band_labels(uci);
        assert_eq!(l["default_radio0"], "Wi-Fi 2.4GHz");
        assert_eq!(l["default_radio1"], "Wi-Fi 5GHz");
    }

    #[test]
    fn fdb_entries_carry_port_and_vlan() {
        let out = "6e:38:73:d3:40:a0 dev phy0-ap0-yyiph vlan 101 master br-lan \n\
                   6c:1f:f7:1d:f6:10 dev eth0 vlan 101 master br-lan \n\
                   18:66:96:a2:62:8c dev br-lan vlan 4094 master br-lan permanent\n\
                   aa:bb:cc:dd:ee:ff dev eth1 self\n";
        let e = fdb_entries(out);
        assert_eq!(e.len(), 2);
        assert_eq!(
            e[0],
            (
                "6E:38:73:D3:40:A0".into(),
                "phy0-ap0-yyiph".into(),
                Some(101)
            )
        );
        assert_eq!(e[1].1, "eth0");
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

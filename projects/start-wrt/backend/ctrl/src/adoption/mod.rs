//! Access point adoption over the backhaul (design D21, D24).
//!
//! A main router announces itself on the backhaul VLAN of its trunk ports with
//! an IPv6 link-local multicast. A candidate — a router in the access point role
//! that has not yet pinned a main router — answers with its identity on its WAN
//! port, or on its backhaul VLAN once it has been an access point, which puts it
//! on the main router's candidate list. The admin then picks it and types its
//! secret at the main router; the [`proof`] exchange runs over the same
//! link-local segment and, on success, both sides provision the tunnel the main
//! router dictated with the keys the exchange pinned and the pre-shared key it
//! derived. Nothing here crosses a router: link-local traffic is confined to one
//! segment.
//!
//! The candidate accepts datagrams from that one port; the main router accepts
//! offers only from a candidate it has sent a hello to.

mod proof;

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use clap::Parser;
use proof::{b64, Adopter, AttemptGuard, Confirm, Hello, Offer, Reject, Responder};
pub use proof::{generate_code, Secret, TunnelSpec, ATTEMPT_LIMIT, CODE_LEN, COOLDOWN};
use rpc_toolkit::{from_fn_async, from_fn_async_local, HandlerExt, ParentHandler};
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use uciedit::openwrt::{FirewallRule, FirewallTarget, NetworkInterface};
use uciedit::{dump_all, parse_all, Arena, Configs};

use crate::access_points::{self, AccessPoint, RouterRole};
use crate::backhaul::backhaul_device;
use crate::prelude::*;
use crate::utils::HandlerExtSerde;
use crate::wg::WgKey;
use crate::{CliContext, CtrlContext, ServerContext};

/// UDP port both sides listen on, link-local only.
pub const PORT: u16 = 8719;
const ANNOUNCE_EVERY: Duration = Duration::from_secs(5);
const CANDIDATE_TTL: Duration = Duration::from_secs(30);
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);
const ALL_NODES: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1);
const MAX_DATAGRAM: usize = 4096;
const CANDIDATE_RULE_NAME: &str = "Allow-Adoption";
const CANDIDATE_RULE_SECTION: &str = "startwrt_adoption";
const LISTEN_PORT_BASE: u16 = 51900;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "body", rename_all = "camelCase")]
enum Message {
    Announce,
    Present(Present),
    Hello(Hello),
    Offer(Offer),
    Confirm(Confirm),
    Reject(Reject),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Present {
    access_point_key: String,
    version: String,
    has_sticker: bool,
}

// ── Main router side ────────────────────────────────────────────────────────

struct Seen {
    present: Present,
    at: Instant,
}

struct MainLink {
    socket: UdpSocket,
    candidates: Mutex<BTreeMap<SocketAddrV6, Seen>>,
    pending: Mutex<HashMap<SocketAddrV6, oneshot::Sender<Message>>>,
    guards: Mutex<HashMap<Ipv6Addr, AttemptGuard>>,
}

static MAIN_LINK: OnceLock<Arc<MainLink>> = OnceLock::new();

/// Starts the side of adoption this router's role calls for. A main router
/// announces and listens for candidates; an unadopted access point waits to be
/// adopted, and so does a router with no password yet (design §19: a fresh or
/// reset router that hears a main router on its uplink joins it instead of
/// becoming one).
pub fn start() {
    let role = access_points::load_role();
    let fresh = role.role == RouterRole::Main
        && std::thread::spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(crate::auth::get_shadow_hash("root"))
                .ok()
                .flatten()
                .is_none()
        })
        .join()
        .unwrap_or(false);
    match role.role {
        RouterRole::Main => {
            tokio::spawn(async {
                if let Err(e) = run_main().await {
                    tracing::error!("adoption: main router listener failed: {e}");
                }
            });
            if fresh {
                start_candidate();
            }
        }
        RouterRole::AccessPoint if role.main_key.is_none() => start_candidate(),
        RouterRole::AccessPoint => {}
    }
}

fn start_candidate() {
    {
        // Provisioning holds a uciedit `Arena` across awaits (!Send).
        if let Err(e) = std::thread::Builder::new()
            .name("adoption".into())
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("adoption runtime")
                    .block_on(async {
                        if let Err(e) = run_candidate().await {
                            tracing::error!("adoption: candidate listener failed: {e}");
                        }
                    });
            })
        {
            tracing::error!("adoption: failed to start candidate thread: {e}");
        }
    }
}

async fn bind() -> Result<UdpSocket, Error> {
    let sock = socket2::Socket::new(
        socket2::Domain::IPV6,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .with_kind(ErrorKind::Network)?;
    sock.set_only_v6(true).with_kind(ErrorKind::Network)?;
    sock.set_reuse_address(true).with_kind(ErrorKind::Network)?;
    sock.set_nonblocking(true).with_kind(ErrorKind::Network)?;
    sock.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, PORT)).into())
        .with_kind(ErrorKind::Network)?;
    UdpSocket::from_std(sock.into()).with_kind(ErrorKind::Network)
}

fn encode(m: &Message) -> Vec<u8> {
    serde_json::to_vec(m).expect("Message serializes")
}

fn if_index(name: &str) -> Option<u32> {
    nix::net::if_::if_nametoindex(name).ok()
}

fn if_name(index: u32) -> Option<String> {
    std::fs::read_dir("/sys/class/net").ok()?.find_map(|e| {
        let e = e.ok()?;
        let n = std::fs::read_to_string(e.path().join("ifindex")).ok()?;
        (n.trim().parse::<u32>().ok()? == index).then(|| e.file_name().into_string().ok())?
    })
}

/// The main router announces on the backhaul VLAN of its LAN bridge alone: a
/// router on a profile port is a client, not a candidate.
fn backhaul_interfaces() -> Vec<String> {
    std::fs::read_dir("/sys/class/net")
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| {
                    n.starts_with("br-") && *n == backhaul_device(n.split('.').next().unwrap_or(""))
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn run_main() -> Result<(), Error> {
    let link = Arc::new(MainLink {
        socket: bind().await?,
        candidates: Mutex::new(BTreeMap::new()),
        pending: Mutex::new(HashMap::new()),
        guards: Mutex::new(HashMap::new()),
    });
    if MAIN_LINK.set(link.clone()).is_err() {
        return Ok(());
    }
    tracing::info!("adoption: main router announcing on the backhaul, port {PORT}");
    let announce = encode(&Message::Announce);
    let mut tick = tokio::time::interval(ANNOUNCE_EVERY);
    let mut buf = vec![0u8; MAX_DATAGRAM];
    loop {
        tokio::select! {
            _ = tick.tick() => {
                for iface in backhaul_interfaces() {
                    let Some(idx) = if_index(&iface) else { continue };
                    let dst = SocketAddrV6::new(ALL_NODES, PORT, 0, idx);
                    if let Err(e) = link.socket.send_to(&announce, dst).await {
                        tracing::debug!("adoption: announce on {iface}: {e}");
                    }
                }
            }
            r = link.socket.recv_from(&mut buf) => {
                let Ok((n, SocketAddr::V6(src))) = r else { continue };
                if !src.ip().is_unicast_link_local() {
                    continue;
                }
                let Ok(msg) = serde_json::from_slice::<Message>(&buf[..n]) else { continue };
                match msg {
                    Message::Present(p) => {
                        link.candidates.lock().unwrap().insert(src, Seen { present: p, at: Instant::now() });
                    }
                    m @ (Message::Offer(_) | Message::Reject(_)) => {
                        if let Some(tx) = link.pending.lock().unwrap().remove(&src) {
                            let _ = tx.send(m);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// A router waiting to be adopted, as the main router sees it.
#[derive(Clone, Debug, Serialize, Deserialize, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// Link-local address with its interface, e.g. `fe80::1%br-lan.1`.
    pub address: String,
    pub interface: String,
    pub access_point_key: String,
    pub version: String,
    pub has_sticker: bool,
    pub last_seen_secs: u64,
}
rpc_toolkit::reflect_ts!(Candidate);

fn main_link() -> Result<Arc<MainLink>, Error> {
    MAIN_LINK.get().cloned().ok_or_else(|| {
        Error::new(
            eyre!("adoption listener is not running on this router"),
            ErrorKind::Uninitialized,
        )
    })
}

pub fn adoption<C: CtrlContext>() -> ParentHandler<C> {
    ParentHandler::new()
        .subcommand(
            "candidates",
            from_fn_async(candidates)
                .with_display_serializable()
                .with_call_remote::<CliContext>(),
        )
        .subcommand(
            "adopt",
            from_fn_async_local(adopt)
                .with_display_serializable()
                .with_call_remote::<CliContext>(),
        )
        .subcommand(
            "code",
            from_fn_async(code)
                .with_display_serializable()
                .with_call_remote::<CliContext>(),
        )
}

#[instrument(skip_all)]
pub async fn candidates(_ctx: ServerContext) -> Result<Vec<Candidate>, Error> {
    access_points::ensure_main()?;
    let link = main_link()?;
    let now = Instant::now();
    let mut seen = link.candidates.lock().unwrap();
    seen.retain(|_, s| now.duration_since(s.at) < CANDIDATE_TTL);
    Ok(seen
        .iter()
        .map(|(addr, s)| {
            let interface = if_name(addr.scope_id()).unwrap_or_default();
            Candidate {
                address: format!("{}%{}", addr.ip(), interface),
                interface,
                access_point_key: s.present.access_point_key.clone(),
                version: s.present.version.clone(),
                has_sticker: s.present.has_sticker,
                last_seen_secs: now.duration_since(s.at).as_secs(),
            }
        })
        .collect())
}

#[derive(Deserialize, Serialize, Parser, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
pub struct AdoptParams {
    /// A candidate's address as `candidates` lists it.
    address: String,
    /// The access point's registry name. An existing entry of that name is
    /// replaced: an access point re-adopted after a factory reset keeps its
    /// name.
    name: String,
    /// The candidate's sticker Wi-Fi password, or its adoption code.
    #[clap(long)]
    secret: String,
    #[clap(long)]
    main_tunnel_addr: Option<String>,
    #[clap(long)]
    access_point_tunnel_addr: Option<String>,
    #[clap(long)]
    listen_port: Option<u16>,
}
rpc_toolkit::reflect_ts!(AdoptParams);

#[derive(Debug, Serialize, Deserialize, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct AdoptResponse {
    pub name: String,
    pub access_point_key: String,
    pub interface: String,
}
rpc_toolkit::reflect_ts!(AdoptResponse);

fn parse_candidate_address(s: &str) -> Result<SocketAddrV6, Error> {
    let (ip, iface) = s.split_once('%').ok_or_else(|| {
        Error::new(
            eyre!("address must be <link-local>%<interface>"),
            ErrorKind::InvalidRequest,
        )
    })?;
    let ip: Ipv6Addr = ip
        .parse()
        .map_err(|_| Error::new(eyre!("invalid address '{ip}'"), ErrorKind::InvalidRequest))?;
    let idx = if_index(iface)
        .ok_or_else(|| Error::new(eyre!("no interface '{iface}'"), ErrorKind::NotFound))?;
    Ok(SocketAddrV6::new(ip, PORT, 0, idx))
}

/// The first UDP port from `base` no WireGuard interface listens on.
fn next_free_listen_port(cfgs: &Configs<'_>, base: u16) -> u16 {
    let used: Vec<u16> = cfgs["network"]
        .sections
        .iter()
        .filter_map(|s| s.get::<crate::vpn_server::WgInterface>().ok())
        .filter_map(|w| w.listen_port)
        .collect();
    (base..=u16::MAX)
        .find(|p| !used.contains(p))
        .unwrap_or(base)
}

/// The main router's tunnel address and listen port for one access point, if
/// provisioned.
fn existing_main_end(cfgs: &Configs<'_>, access_point: &str) -> Option<(String, u16)> {
    let interface = crate::mgmt_tunnel::ap_interface_name(access_point);
    let wg = cfgs["network"]
        .sections
        .iter()
        .find(|s| s.name().as_deref() == Some(interface.as_str()))
        .and_then(|s| s.get::<crate::vpn_server::WgInterface>().ok())?;
    let addr = wg.addresses.first()?.split('/').next()?.to_string();
    Some((addr, wg.listen_port?))
}

fn next_transit_index(registry: &[AccessPoint]) -> u8 {
    registry
        .iter()
        .filter_map(|s| s.mgmt_address.parse::<std::net::Ipv4Addr>().ok())
        .filter(|a| a.octets()[..2] == access_points::TRANSIT_BASE)
        .map(|a| a.octets()[2])
        .max()
        .map_or(1, |n| n.saturating_add(1))
}

#[instrument(skip_all)]
pub async fn adopt(_ctx: ServerContext, p: AdoptParams) -> Result<AdoptResponse, Error> {
    access_points::ensure_main()?;
    let link = main_link()?;
    let target = parse_candidate_address(&p.address)?;
    let iface = if_name(target.scope_id()).unwrap_or_default();
    {
        let mut guards = link.guards.lock().unwrap();
        if let Some(r) = guards
            .entry(*target.ip())
            .or_default()
            .check(Instant::now())
        {
            return Err(Error::new(eyre!("{r}"), ErrorKind::RateLimited));
        }
    }

    // D20: the country is the main router's and travels in the payload.
    if crate::wifi::get(ServerContext::default())
        .await
        .map(|w| w.country.is_none())
        .unwrap_or(false)
    {
        return Err(Error::new(
            eyre!("set the main router's Wi-Fi country before adopting an access point"),
            ErrorKind::InvalidRequest,
        ));
    }
    let registry_guard = access_points::REGISTRY_LOCK.lock().await;
    let registry = access_points::load_registry();
    // A same-name re-adoption keeps the tunnel's addresses and port.
    let existing = registry.iter().find(|s| s.name == p.name);
    let (listen_port, main_existing) = {
        let arena = Arena::new();
        let cfgs = parse_all("/etc/config", &arena, &["network"]).await?;
        let current = existing.and_then(|_| existing_main_end(&cfgs, &p.name));
        let port = p
            .listen_port
            .or_else(|| current.as_ref().map(|c| c.1))
            .unwrap_or_else(|| next_free_listen_port(&cfgs, LISTEN_PORT_BASE));
        (port, current.map(|c| c.0))
    };
    let (main_default, ap_default) = match (existing, main_existing) {
        (Some(e), Some(addr)) => (addr, e.mgmt_address.clone()),
        _ => {
            let (m, a) = access_points::transit_addrs(next_transit_index(&registry));
            (m.to_string(), a.to_string())
        }
    };
    let tunnel = TunnelSpec {
        access_point: p.name.clone(),
        main_listen_port: listen_port,
        main_tunnel_addr: p.main_tunnel_addr.clone().unwrap_or(main_default),
        access_point_tunnel_addr: p.access_point_tunnel_addr.clone().unwrap_or(ap_default),
    };

    let main_key = WgKey::generate();
    let mut adopter = Adopter::new(Secret::new(p.secret), main_key.public_key());
    let (tx, rx) = oneshot::channel();
    link.pending.lock().unwrap().insert(target, tx);
    link.socket
        .send_to(&encode(&Message::Hello(adopter.hello())), target)
        .await
        .with_kind(ErrorKind::Network)?;
    let reply = tokio::time::timeout(EXCHANGE_TIMEOUT, rx).await;
    link.pending.lock().unwrap().remove(&target);
    let offer = match reply {
        Ok(Ok(Message::Offer(o))) => o,
        Ok(Ok(Message::Reject(r))) => {
            return Err(Error::new(
                eyre!("the router refused: {r}"),
                ErrorKind::RateLimited,
            ))
        }
        _ => {
            return Err(Error::new(
                eyre!(
                    "no answer from {} within {} s",
                    p.address,
                    EXCHANGE_TIMEOUT.as_secs()
                ),
                ErrorKind::Timeout,
            ))
        }
    };

    let now = Instant::now();
    let accepted = {
        let mut guards = link.guards.lock().unwrap();
        let guard = guards.entry(*target.ip()).or_default();
        adopter.accept(&offer, tunnel.clone(), guard, now)
    };
    let (confirm, adopted) = match accepted {
        Ok(ok) => ok,
        Err(r) => {
            let _ = link
                .socket
                .send_to(&encode(&Message::Reject(r.clone())), target)
                .await;
            crate::activity::log(
                "access-point",
                "adopt",
                false,
                &format!("Adoption of '{}' refused: {r}", p.name),
                None,
            );
            let kind = match r {
                Reject::Locked { .. } => ErrorKind::RateLimited,
                _ => ErrorKind::IncorrectPassword,
            };
            return Err(Error::new(eyre!("{r}"), kind));
        }
    };
    link.socket
        .send_to(&encode(&Message::Confirm(confirm)), target)
        .await
        .with_kind(ErrorKind::Network)?;

    let access_point_key = b64(adopted.access_point_key.as_bytes());
    let preshared_key = b64(&adopted.preshared_key);
    let main_private_key = b64(main_key.as_ref());
    let main_addr = parse_ip(&tunnel.main_tunnel_addr)?;
    let access_point_addr = parse_ip(&tunnel.access_point_tunnel_addr)?;
    {
        let arena = Arena::new();
        let mut cfgs = parse_all("/etc/config", &arena, &["network", "firewall", "system"]).await?;
        crate::mgmt_tunnel::provision_main_end(
            &mut cfgs,
            &arena,
            &crate::mgmt_tunnel::MainEnd {
                access_point: &p.name,
                main_addr,
                access_point_addr,
                access_point_public_key: &access_point_key,
                preshared_key: &preshared_key,
                listen_port,
                main_private_key: &main_private_key,
            },
        )?;
        crate::mgmt_tunnel::ensure_main_ntp_server(&mut cfgs)?;
        dump_all("/etc/config", cfgs).await?;
    }
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
    let mut registry = registry;
    // A saved configuration (design §19): re-adopting a name keeps its port
    // assignments; everything else the new router reports afresh.
    let saved_ports = registry
        .iter()
        .find(|s| s.name == p.name)
        .map(|s| s.ports.clone())
        .unwrap_or_default();
    access_points::remove_access_point(&mut registry, &p.name);
    access_points::add_access_point(
        &mut registry,
        AccessPoint {
            name: p.name.clone(),
            public_key: access_point_key.clone(),
            mgmt_address: tunnel.access_point_tunnel_addr.clone(),
            ports: saved_ports,
            ..Default::default()
        },
    )?;
    access_points::persist_registry(&registry).await?;
    drop(registry_guard);
    crate::activity::log(
        "access-point",
        "adopt",
        true,
        &format!("Adopted access point '{}' on {iface}", p.name),
        None,
    );
    Ok(AdoptResponse {
        name: p.name,
        access_point_key,
        interface: crate::mgmt_tunnel::ap_interface_name(&tunnel.access_point),
    })
}

// ── Candidate side ──────────────────────────────────────────────────────────

struct CandidateState {
    code: Option<String>,
    has_sticker: bool,
    device: String,
    heard: Option<Instant>,
}

/// A main router announcing within this long ago is still there.
const HEARD_FRESH: Duration = Duration::from_secs(15);

static CANDIDATE: Mutex<Option<CandidateState>> = Mutex::new(None);

/// What the pre-adoption page shows.
#[derive(Debug, Serialize, Deserialize, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct CodeResponse {
    /// Listening for a main router.
    pub waiting: bool,
    /// The sticker Wi-Fi password is the secret; no code is shown.
    pub has_sticker: bool,
    pub code: Option<String>,
    /// A main router announced itself on the port it listens on.
    pub heard_main: bool,
    /// A cable with a link is in that port.
    pub link: bool,
    /// The router has a default route.
    pub internet: bool,
}
rpc_toolkit::reflect_ts!(CodeResponse);

#[instrument(skip_all)]
pub async fn code(_ctx: ServerContext) -> Result<CodeResponse, Error> {
    let adopted = access_points::load_role().main_key.is_some();
    let state = CANDIDATE
        .lock()
        .unwrap()
        .as_ref()
        .filter(|_| !adopted)
        .map(|s| {
            (
                s.has_sticker,
                s.code.clone(),
                s.device.clone(),
                s.heard.is_some_and(|t| t.elapsed() < HEARD_FRESH),
            )
        });
    Ok(match state {
        Some((has_sticker, code, device, heard_main)) => CodeResponse {
            waiting: true,
            has_sticker,
            code,
            heard_main,
            link: carrier(&device),
            internet: default_route().await,
        },
        None => CodeResponse {
            waiting: false,
            has_sticker: false,
            code: None,
            heard_main: false,
            link: false,
            internet: false,
        },
    })
}

fn carrier(device: &str) -> bool {
    std::fs::read_to_string(format!("/sys/class/net/{device}/carrier"))
        .is_ok_and(|c| c.trim() == "1")
}

async fn default_route() -> bool {
    for family in ["-4", "-6"] {
        let out = tokio::process::Command::new("ip")
            .args([family, "route", "show", "default"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .await;
        if out.is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty()) {
            return true;
        }
    }
    false
}

fn wan_device(cfgs: &Configs<'_>) -> Option<String> {
    cfgs["network"]
        .sections
        .iter()
        .find(|s| s.name().as_deref() == Some(crate::wan::WAN_INTERFACE))
        .and_then(|s| s.get::<NetworkInterface>().ok())
        .map(|i| i.device)
        .filter(|d| !d.is_empty())
}

/// Where a candidate listens: its WAN port on a fresh router, the backhaul
/// VLAN on one that has been an access point and was released. The second
/// is `false`: the backhaul zone already admits adoption.
fn candidate_device(cfgs: &Configs<'_>) -> Option<(String, bool)> {
    if let Some(wan) = wan_device(cfgs) {
        return Some((wan, true));
    }
    let has_backhaul = cfgs["network"]
        .sections
        .iter()
        .any(|s| s.name().as_deref() == Some(crate::backhaul::BACKHAUL_INTERFACE));
    if !has_backhaul {
        return None;
    }
    let bridge = crate::ethernet::find_lan_bridge(cfgs)
        .ok()
        .flatten()
        .map(|d| d.name)
        .unwrap_or_else(|| crate::ethernet::DEFAULT_LAN_BRIDGE.to_string());
    Some((backhaul_device(&bridge), false))
}

fn ensure_candidate_firewall_rule(cfgs: &mut Configs<'_>) -> Result<bool, Error> {
    let exists = cfgs["firewall"]
        .sections
        .iter()
        .filter_map(|s| s.get::<FirewallRule>().ok())
        .any(|r| r.name == CANDIDATE_RULE_NAME);
    if exists {
        return Ok(false);
    }
    let rule = FirewallRule {
        name: CANDIDATE_RULE_NAME.to_string(),
        src: crate::wan::WAN_INTERFACE.to_string(),
        src_ip: Some("fe80::/10".to_string()),
        dest_port: Some(PORT.to_string()),
        proto: vec!["udp".to_string()],
        target: FirewallTarget::ACCEPT,
        family: Some("ipv6".to_string()),
        ..Default::default()
    };
    cfgs["firewall"].append(&rule, Some(CANDIDATE_RULE_SECTION))?;
    Ok(true)
}

async fn run_candidate() -> Result<(), Error> {
    let (secret, has_sticker, code) = match crate::eeprom::read_wifi_password()? {
        Some(p) => (Secret::new(p), true, None),
        None => {
            let c = generate_code();
            (Secret::new(c.clone()), false, Some(c))
        }
    };
    let wan = {
        let arena = Arena::new();
        let mut cfgs = parse_all("/etc/config", &arena, &["network", "firewall"]).await?;
        let (device, on_wan) = candidate_device(&cfgs).ok_or_else(|| {
            Error::new(
                eyre!("neither a WAN device nor a backhaul is configured"),
                ErrorKind::Network,
            )
        })?;
        if on_wan && ensure_candidate_firewall_rule(&mut cfgs)? {
            dump_all("/etc/config", cfgs).await?;
            let _ = crate::run_quiet_async(
                tokio::process::Command::new("/etc/init.d/firewall").arg("reload"),
            )
            .await;
        }
        device
    };
    let socket = bind().await?;
    let key = WgKey::generate();
    let mut responder = Responder::new(secret, key.public_key());
    let mut guard = AttemptGuard::default();
    let present = Present {
        access_point_key: b64(key.public_key().as_bytes()),
        version: env!("CARGO_PKG_VERSION").to_string(),
        has_sticker,
    };
    *CANDIDATE.lock().unwrap() = Some(CandidateState {
        code,
        has_sticker,
        device: wan.clone(),
        heard: None,
    });
    tracing::info!("adoption: waiting for a main router on {wan}, port {PORT}");

    let mut buf = vec![0u8; MAX_DATAGRAM];
    loop {
        let Ok((n, SocketAddr::V6(src))) = socket.recv_from(&mut buf).await else {
            continue;
        };
        if !src.ip().is_unicast_link_local() || Some(src.scope_id()) != if_index(&wan) {
            continue;
        }
        let Ok(msg) = serde_json::from_slice::<Message>(&buf[..n]) else {
            continue;
        };
        let now = Instant::now();
        let reply = match msg {
            Message::Announce => {
                if let Some(s) = CANDIDATE.lock().unwrap().as_mut() {
                    s.heard = Some(now);
                }
                Message::Present(present.clone())
            }
            Message::Hello(h) => match responder.respond(&h, &mut guard, now) {
                Ok(o) => Message::Offer(o),
                Err(r) => Message::Reject(r),
            },
            Message::Confirm(c) => match responder.confirm(&c, &mut guard, now) {
                Ok(by) => {
                    let endpoint = format!("{}%{}", src.ip(), wan);
                    match adopt_self(&key, &by, src.ip()).await {
                        Ok(()) => {
                            crate::activity::log(
                                "access-point",
                                "adopted",
                                true,
                                &format!(
                                    "Adopted by the main router at {endpoint} as '{}'",
                                    by.tunnel.access_point
                                ),
                                None,
                            );
                            tracing::info!("adoption: adopted by {endpoint}; listener stopping");
                            return Ok(());
                        }
                        Err(e) => {
                            tracing::error!("adoption: provisioning after adoption failed: {e}");
                            crate::activity::log(
                                "access-point",
                                "adopted",
                                false,
                                "Adoption succeeded but provisioning failed",
                                Some(&e.to_string()),
                            );
                            continue;
                        }
                    }
                }
                Err(r) => {
                    tracing::warn!("adoption: refused a main router at {}: {r}", src.ip());
                    Message::Reject(r)
                }
            },
            _ => continue,
        };
        if let Err(e) = socket.send_to(&encode(&reply), src).await {
            tracing::debug!("adoption: reply to {}: {e}", src.ip());
        }
    }
}

fn parse_ip(s: &str) -> Result<std::net::IpAddr, Error> {
    s.parse().map_err(|_| {
        Error::new(
            eyre!("invalid tunnel address '{s}'"),
            ErrorKind::InvalidRequest,
        )
    })
}

/// Becoming an access point: the uplink joins the bridge, the main router's
/// link-local endpoint is reached through the backhaul VLAN from here on, and
/// the management tunnel dials it.
async fn adopt_self(key: &WgKey, by: &proof::AdoptedBy, main_ip: &Ipv6Addr) -> Result<(), Error> {
    let t = &by.tunnel;
    let endpoint;
    let uplink;
    {
        let arena = Arena::new();
        let mut cfgs = parse_all(
            "/etc/config",
            &arena,
            &["network", "firewall", "dhcp", "wireless", "system"],
        )
        .await?;
        // A released access point has no WAN; its uplink is on record.
        uplink = crate::ap::uplink_from_wan(&cfgs)
            .or_else(|| access_points::load_role().uplink)
            .ok_or_else(|| Error::new(eyre!("no uplink port known"), ErrorKind::Network))?;
        crate::ap::write_base_layout(&mut cfgs, &uplink)?;
        let bridge = crate::ethernet::find_lan_bridge(&cfgs)?
            .map(|d| d.name)
            .unwrap_or_else(|| crate::ethernet::DEFAULT_LAN_BRIDGE.to_string());
        endpoint = format!("{main_ip}%{}", backhaul_device(&bridge));
        crate::mgmt_tunnel::provision_ap_end(
            &mut cfgs,
            &arena,
            &crate::mgmt_tunnel::ApEnd {
                access_point_addr: parse_ip(&t.access_point_tunnel_addr)?,
                main_addr: parse_ip(&t.main_tunnel_addr)?,
                main_public_key: &b64(by.main_key.as_bytes()),
                main_endpoint_host: &endpoint,
                main_endpoint_port: t.main_listen_port,
                preshared_key: &b64(&by.preshared_key),
                private_key: &b64(key.as_ref()),
            },
        )?;
        dump_all("/etc/config", cfgs).await?;
    }
    let mut role = access_points::load_role();
    role.role = RouterRole::AccessPoint;
    role.main_endpoint = Some(endpoint);
    role.main_key = Some(b64(by.main_key.as_bytes()));
    role.main_address = Some(t.main_tunnel_addr.clone());
    role.name = Some(t.access_point.clone());
    role.uplink = Some(uplink.clone());
    role.generation = 0;
    access_points::persist_role(&role).await?;
    if let Err(e) = crate::bridge_filters::sync_ap(&uplink).await {
        tracing::error!("adoption: bridge filters not written: {e}");
    }
    crate::ap::reload_all().await;
    let _ =
        crate::run_quiet_async(tokio::process::Command::new("/etc/init.d/sysntpd").arg("restart"))
            .await;
    crate::liveness::start();
    crate::control::start();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ap(addr: &str) -> AccessPoint {
        AccessPoint {
            name: "s".into(),
            public_key: "k".into(),
            mgmt_address: addr.into(),
            ..Default::default()
        }
    }

    #[test]
    fn transit_index_follows_the_registry() {
        assert_eq!(next_transit_index(&[]), 1);
        assert_eq!(next_transit_index(&[ap("10.42.1.2")]), 2);
        assert_eq!(next_transit_index(&[ap("10.42.3.2"), ap("10.42.1.2")]), 4);
        assert_eq!(next_transit_index(&[ap("192.168.130.2")]), 1);
    }

    #[test]
    fn candidate_address_needs_an_interface() {
        assert!(parse_candidate_address("fe80::1").is_err());
        assert!(parse_candidate_address("nope%lo").is_err());
        let a = parse_candidate_address("fe80::1%lo").unwrap();
        assert_eq!(a.port(), PORT);
        assert_eq!(a.scope_id(), if_index("lo").unwrap());
    }

    #[test]
    fn messages_are_adjacently_tagged() {
        let s = serde_json::to_string(&Message::Announce).unwrap();
        assert_eq!(s, r#"{"type":"announce"}"#);
        let r = Message::Reject(Reject::NoExchange);
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, r#"{"type":"reject","body":{"reason":"noExchange"}}"#);
        assert!(matches!(
            serde_json::from_str::<Message>(&s).unwrap(),
            Message::Reject(Reject::NoExchange)
        ));
    }

    #[tokio::test]
    async fn candidate_rule_is_added_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("network"), "").unwrap();
        std::fs::write(
            dir.path().join("firewall"),
            "config zone\n\toption name 'wan'\n\tlist network 'wan'\n\toption input 'REJECT'\n\
             \toption output 'ACCEPT'\n\toption forward 'REJECT'\n",
        )
        .unwrap();
        let arena = Arena::new();
        let mut cfgs = parse_all(dir.path(), &arena, &["network", "firewall"])
            .await
            .unwrap();
        assert!(ensure_candidate_firewall_rule(&mut cfgs).unwrap());
        assert!(!ensure_candidate_firewall_rule(&mut cfgs).unwrap());
        let rule = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<FirewallRule>().ok())
            .find(|r| r.name == CANDIDATE_RULE_NAME)
            .unwrap();
        assert_eq!(rule.src, "wan");
        assert_eq!(rule.dest_port.as_deref(), Some("8719"));
        assert_eq!(rule.family.as_deref(), Some("ipv6"));
    }

    #[tokio::test]
    async fn wan_device_and_free_port_resolve_from_configs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("network"),
            "config interface 'wan'\n\toption device 'eth1'\n\toption proto 'dhcp'\n\n\
             config interface 'lan'\n\toption device 'br-lan.1'\n\toption proto 'static'\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("firewall"),
            "config zone\n\toption name 'lan'\n\tlist network 'lan'\n\toption input 'ACCEPT'\n\
             \toption output 'ACCEPT'\n\toption forward 'ACCEPT'\n",
        )
        .unwrap();
        let arena = Arena::new();
        let cfgs = parse_all(dir.path(), &arena, &["network", "firewall"])
            .await
            .unwrap();
        assert_eq!(wan_device(&cfgs).as_deref(), Some("eth1"));
        assert_eq!(next_free_listen_port(&cfgs, 51900), 51900);
    }
}

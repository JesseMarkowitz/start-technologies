//! Satellite router support — router **role** and the Core's **paired-satellite
//! registry**.
//!
//! This is the foundation slice of the satellite-router feature (design:
//! `projects/start-wrt/docs/design/satellite-router.md`). A StartWRT router is
//! either a **Core** (owns the profiles, holds the only WAN, source of truth) or
//! a **Satellite** (a follower that extends Wi-Fi/Ethernet coverage and reaches
//! the Core over per-profile WireGuard tunnels). The role is chosen at
//! provisioning and persisted here; the Core keeps a registry of the satellites
//! it has paired with.
//!
//! ## Implemented here
//! - `RouterRole` persisted to `/etc/startwrt/role.json` (default: Core).
//! - The paired-satellite registry at `/etc/startwrt/satellites.json`.
//! - RPC: `satellite.{get-role,set-role,list,pair,unpair,status}`.
//! - Core-only capability gating for the registry operations (a first slice of
//!   the role-aware gating in the design's D7).
//!
//! ## Deliberately NOT here yet (tracked in the design doc + NEXT_STEPS)
//! - The site-to-site WireGuard provisioning (`vpn_site`), the WAN-less egress,
//!   the enrollment-token trust bootstrap and remote-peer auth (D4), and the
//!   Core→satellite semantic config-sync transport (D5). `pair` therefore records
//!   a satellite in the registry only; it does **not** yet bring up tunnels or
//!   push config, and says so in its response rather than implying otherwise.

use std::net::Ipv4Addr;
use std::path::Path;

use clap::Parser;
use rpc_toolkit::{from_fn_async, from_fn_async_local, HandlerExt, ParentHandler};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use uciedit::{dump_all, parse_all, Arena};

use crate::prelude::*;
use crate::utils::HandlerExtSerde;
use crate::{CliContext, CtrlContext, ServerContext};

const DIR: &str = "/etc/startwrt";
const ROLE_PATH: &str = "/etc/startwrt/role.json";
const REGISTRY_PATH: &str = "/etc/startwrt/satellites.json";

/// Which role this physical router plays. Chosen at provisioning; a router with
/// no marker is a standalone **Core** (the common case).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RouterRole {
    #[default]
    Core,
    Satellite,
}

/// Persisted role marker (`/etc/startwrt/role.json`).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RoleMarker {
    pub role: RouterRole,
    /// On a Satellite, the Core's reachable endpoint on the inter-router link
    /// (underlay address). `None` on a Core.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_endpoint: Option<String>,
}

/// One satellite the Core has paired with (`/etc/startwrt/satellites.json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedSatellite {
    /// Admin-facing label (unique).
    pub name: String,
    /// The satellite's WireGuard public key (base64), pinned at pairing — the
    /// far end of the management tunnel and the trust anchor for its RPC.
    pub public_key: String,
    /// The satellite's management address on the inter-router link.
    pub mgmt_address: String,
    /// The config generation this satellite last acknowledged (0 = never). Used
    /// to surface staleness after a config edit (see design §10).
    #[serde(default)]
    pub generation_applied: u64,
    /// Unix seconds of last successful contact (0 = never).
    #[serde(default)]
    pub last_seen: i64,
}

// ── Config-sync payload (Core → satellite, design D5) ───────────────────────
//
// The Core pushes this *semantic* snapshot (not raw UCI, not a full backup); the
// satellite regenerates its own subnet/DHCP/zone/routing locally from it. This
// sidesteps clobbering satellite-local identity and is the minimal cross-router
// set the design identified. The transport (push over the management tunnel) and
// the Core-side builder from `profiles`/`wifi`/`ethernet` are follow-up work; the
// types + allocators below are the contract and the pairing primitives.

/// A complete, Core-authored config snapshot. `generation` is monotonic — a
/// satellite rejects a snapshot older than the one it has applied (anti-rollback).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SyncSnapshot {
    pub generation: u64,
    pub ssid: String,
    pub admin_key: String,
    pub profiles: Vec<ProfileSpec>,
    pub passwords: Vec<PasswordSpec>,
    pub ports: Vec<PortSpec>,
}

/// One profile as the satellite needs to reconstruct it locally.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSpec {
    pub interface: String,
    pub vlan_tag: u16,
    /// The satellite-local `/24` (CIDR) the Core allocated for this profile here.
    pub subnet: String,
    pub wan_access: String,
    pub outbound: String,
}

/// One Wi-Fi password → profile mapping (per-PSK dynamic VLAN, no RADIUS).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PasswordSpec {
    pub label: String,
    pub key: String,
    pub vlan_tag: u16,
}

/// One Ethernet port → profile mapping on the satellite.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortSpec {
    pub port: String,
    pub vlan_tag: u16,
}

/// Reserved inter-router transit block. Each satellite link gets its own `/24`
/// slice here (`10.42.<index>.0/24`), deliberately disjoint from any profile
/// `/24` so the underlay never collides with a profile subnet.
pub const TRANSIT_BASE: [u8; 2] = [10, 42];

/// The (Core, satellite) transit addresses for satellite `index`
/// (`10.42.<index>.1` / `.2`). Supports up to 254 satellites.
pub fn transit_addrs(index: u8) -> (Ipv4Addr, Ipv4Addr) {
    (
        Ipv4Addr::new(TRANSIT_BASE[0], TRANSIT_BASE[1], index, 1),
        Ipv4Addr::new(TRANSIT_BASE[0], TRANSIT_BASE[1], index, 2),
    )
}

/// Allocate `count` free UDP listen ports for a satellite's per-profile tunnels,
/// starting at `base` and skipping any already in `used`. Pure.
pub fn allocate_listen_ports(used: &[u16], count: usize, base: u16) -> Vec<u16> {
    let mut out = Vec::with_capacity(count);
    let mut p = base;
    while out.len() < count {
        if !used.contains(&p) && !out.contains(&p) {
            out.push(p);
        }
        match p.checked_add(1) {
            Some(next) => p = next,
            None => break, // exhausted the port space
        }
    }
    out
}

// ── RPC surface ─────────────────────────────────────────────────────────────

pub fn satellite<C: CtrlContext>() -> ParentHandler<C> {
    ParentHandler::new()
        .subcommand(
            "get-role",
            from_fn_async(get_role)
                .with_display_serializable()
                .with_call_remote::<CliContext>(),
        )
        .subcommand(
            "set-role",
            from_fn_async(set_role)
                .no_display()
                .with_call_remote::<CliContext>(),
        )
        .subcommand(
            "list",
            from_fn_async(list)
                .with_display_serializable()
                .with_call_remote::<CliContext>(),
        )
        .subcommand(
            "pair",
            from_fn_async(pair)
                .with_display_serializable()
                .with_call_remote::<CliContext>(),
        )
        .subcommand(
            "unpair",
            from_fn_async(unpair)
                .no_display()
                .with_call_remote::<CliContext>(),
        )
        .subcommand(
            "status",
            from_fn_async(status)
                .with_display_serializable()
                .with_call_remote::<CliContext>(),
        )
        .subcommand(
            "provision-core-tunnel",
            from_fn_async_local(provision_core_tunnel)
                .no_display()
                .with_call_remote::<CliContext>(),
        )
        .subcommand(
            "provision-satellite-tunnel",
            from_fn_async_local(provision_satellite_tunnel)
                .no_display()
                .with_call_remote::<CliContext>(),
        )
}

#[instrument(skip_all)]
pub async fn get_role(_ctx: ServerContext) -> Result<RoleMarker, Error> {
    Ok(load_role())
}

#[derive(Deserialize, Serialize, Parser)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
pub struct SetRoleParams {
    /// "core" or "satellite".
    role: String,
    /// On a satellite, the Core's underlay endpoint.
    #[clap(long)]
    core_endpoint: Option<String>,
}

#[instrument(skip_all)]
pub async fn set_role(
    _ctx: ServerContext,
    SetRoleParams {
        role,
        core_endpoint,
    }: SetRoleParams,
) -> Result<(), Error> {
    let role = parse_role(&role)?;
    let marker = RoleMarker {
        role,
        core_endpoint,
    };
    persist_json(ROLE_PATH, &marker).await?;
    crate::activity::log(
        "satellite",
        "set-role",
        true,
        &format!("Router role set to {role:?}"),
        None,
    );
    Ok(())
}

#[instrument(skip_all)]
pub async fn list(_ctx: ServerContext) -> Result<Vec<PairedSatellite>, Error> {
    ensure_core()?;
    Ok(load_registry())
}

#[derive(Deserialize, Serialize, Parser)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
pub struct PairParams {
    name: String,
    /// The satellite's WireGuard public key (base64).
    public_key: String,
    /// The satellite's management address on the inter-router link.
    mgmt_address: String,
    /// Single-use enrollment token (validation is a follow-up — see note).
    #[clap(long)]
    enrollment_token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairResponse {
    pub name: String,
    pub status: String,
    pub note: String,
}

#[instrument(skip_all)]
pub async fn pair(
    _ctx: ServerContext,
    PairParams {
        name,
        public_key,
        mgmt_address,
        enrollment_token: _,
    }: PairParams,
) -> Result<PairResponse, Error> {
    ensure_core()?;
    // TODO(satellite, D4/D5): validate a single-use, short-lived enrollment token;
    // provision the per-profile WireGuard tunnels (vpn_site) and the WAN-less
    // egress; push the initial semantic config snapshot. This handler currently
    // records the satellite in the Core's registry only.
    let mut list = load_registry();
    add_satellite(
        &mut list,
        PairedSatellite {
            name: name.clone(),
            public_key,
            mgmt_address,
            generation_applied: 0,
            last_seen: 0,
        },
    )?;
    persist_json(REGISTRY_PATH, &list).await?;
    crate::activity::log(
        "satellite",
        "paired",
        true,
        &format!("Registered satellite '{name}'"),
        None,
    );
    Ok(PairResponse {
        name,
        status: "registered".to_string(),
        note: "Recorded in the Core registry. Tunnel provisioning and config sync \
               are not yet implemented (see docs/design/satellite-router.md)."
            .to_string(),
    })
}

#[derive(Deserialize, Serialize, Parser)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
pub struct UnpairParams {
    name: String,
}

#[instrument(skip_all)]
pub async fn unpair(
    _ctx: ServerContext,
    UnpairParams { name }: UnpairParams,
) -> Result<(), Error> {
    ensure_core()?;
    let mut list = load_registry();
    if !remove_satellite(&mut list, &name) {
        return Err(Error::new(
            eyre!("no paired satellite named '{name}'"),
            ErrorKind::NotFound,
        ));
    }
    persist_json(REGISTRY_PATH, &list).await?;
    // TODO(satellite): also tear down this satellite's tunnels and revoke its
    // management-tunnel auth so the revocation is effective immediately (design §12 #6).
    crate::activity::log(
        "satellite",
        "unpaired",
        true,
        &format!("Removed satellite '{name}'"),
        None,
    );
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponse {
    pub role: RouterRole,
    pub core_endpoint: Option<String>,
    pub satellite_count: usize,
    pub satellites: Vec<PairedSatellite>,
}

#[instrument(skip_all)]
pub async fn status(_ctx: ServerContext) -> Result<StatusResponse, Error> {
    let marker = load_role();
    // Only a Core has a registry; a satellite reports an empty one.
    let satellites = if marker.role == RouterRole::Core {
        load_registry()
    } else {
        Vec::new()
    };
    Ok(StatusResponse {
        role: marker.role,
        core_endpoint: marker.core_endpoint,
        satellite_count: satellites.len(),
        satellites,
    })
}

// ── Manual tunnel provisioning (for the basic hardware bring-up test) ────────
//
// These apply the `vpn_site` config generators to `/etc/config` and bring the
// tunnel up. They are the executable path for the hardware runbook
// (`docs/design/satellite-router-hardware-test.md`): an operator generates keys,
// picks a transit subnet, and runs these on each router. The eventual pairing
// flow (D4) will call the same generators with allocated keys/ports.

#[derive(Deserialize, Serialize, Parser)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
pub struct ProvisionCoreParams {
    satellite: String,
    profile: String,
    #[clap(long)]
    core_transit_addr: String,
    /// CIDR to route to the satellite over the tunnel (e.g. the satellite's /24,
    /// or its wg host /32 for a self-ping test).
    #[clap(long)]
    satellite_allowed_ip: String,
    #[clap(long)]
    satellite_public_key: String,
    #[clap(long)]
    preshared_key: String,
    #[clap(long)]
    listen_port: u16,
    /// Firewall zone the handshake arrives on (the transit-link zone).
    #[clap(long)]
    transit_zone: String,
    #[clap(long)]
    core_private_key: String,
}

#[instrument(skip_all)]
pub async fn provision_core_tunnel(
    _ctx: ServerContext,
    p: ProvisionCoreParams,
) -> Result<(), Error> {
    ensure_core()?;
    let core_transit_addr: Ipv4Addr = p.core_transit_addr.parse().map_err(|_| {
        Error::new(
            eyre!("invalid core_transit_addr '{}'", p.core_transit_addr),
            ErrorKind::InvalidRequest,
        )
    })?;
    let iface = crate::vpn_site::site_interface_name(&p.satellite, &p.profile);

    let mut retries = 4;
    loop {
        let arena = Arena::new();
        let mut cfgs = parse_all("/etc/config", &arena, &["network", "startwrt", "firewall"]).await?;
        let params = crate::vpn_site::SiteTunnelParams {
            satellite: &p.satellite,
            profile_interface: &p.profile,
            core_transit_addr,
            satellite_subnet: &p.satellite_allowed_ip,
            satellite_public_key: &p.satellite_public_key,
            preshared_key: &p.preshared_key,
            listen_port: p.listen_port,
            transit_zone: &p.transit_zone,
            core_private_key: &p.core_private_key,
        };
        crate::vpn_site::provision_core_site_tunnel(&mut cfgs, &arena, &params)?;
        match dump_all("/etc/config", cfgs).await {
            Err(uciedit::Error::Conflict { .. }) if retries > 0 => {
                retries -= 1;
                continue;
            }
            Err(err) => return Err(err.into()),
            Ok(()) => {
                let _ = crate::run_quiet_async(tokio::process::Command::new("ifup").arg(&iface)).await;
                crate::profiles::reload_system().await?;
                break;
            }
        }
    }
    crate::activity::log(
        "satellite",
        "provision-core-tunnel",
        true,
        &format!(
            "Provisioned Core tunnel for satellite '{}' profile '{}'",
            p.satellite, p.profile
        ),
        None,
    );
    Ok(())
}

#[derive(Deserialize, Serialize, Parser)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
pub struct ProvisionSatelliteParams {
    satellite: String,
    profile: String,
    #[clap(long)]
    sat_wg_addr: String,
    #[clap(long)]
    core_public_key: String,
    #[clap(long)]
    core_endpoint_host: String,
    #[clap(long)]
    core_endpoint_port: u16,
    #[clap(long)]
    preshared_key: String,
    #[clap(long)]
    sat_private_key: String,
    /// Repeatable. What to route through the tunnel; defaults to full egress via
    /// the Core (`0.0.0.0/0`) when omitted.
    #[clap(long)]
    allowed_ip: Vec<String>,
    /// A local interface whose firewall zone the tunnel joins.
    #[clap(long, default_value = "lan")]
    firewall_zone_member: String,
}

#[instrument(skip_all)]
pub async fn provision_satellite_tunnel(
    _ctx: ServerContext,
    p: ProvisionSatelliteParams,
) -> Result<(), Error> {
    ensure_satellite()?;
    let sat_wg_addr: Ipv4Addr = p.sat_wg_addr.parse().map_err(|_| {
        Error::new(
            eyre!("invalid sat_wg_addr '{}'", p.sat_wg_addr),
            ErrorKind::InvalidRequest,
        )
    })?;
    let allowed = if p.allowed_ip.is_empty() {
        vec!["0.0.0.0/0".to_string()]
    } else {
        p.allowed_ip.clone()
    };
    let iface = crate::vpn_site::site_interface_name(&p.satellite, &p.profile);

    let mut retries = 4;
    loop {
        let arena = Arena::new();
        let mut cfgs = parse_all("/etc/config", &arena, &["network", "startwrt", "firewall"]).await?;
        let params = crate::vpn_site::SatelliteTunnelParams {
            satellite: &p.satellite,
            profile_interface: &p.profile,
            sat_wg_addr,
            core_public_key: &p.core_public_key,
            core_endpoint_host: &p.core_endpoint_host,
            core_endpoint_port: p.core_endpoint_port,
            preshared_key: &p.preshared_key,
            sat_private_key: &p.sat_private_key,
            allowed_ips: &allowed,
            firewall_zone_member: &p.firewall_zone_member,
        };
        crate::vpn_site::provision_satellite_site_tunnel(&mut cfgs, &arena, &params)?;
        match dump_all("/etc/config", cfgs).await {
            Err(uciedit::Error::Conflict { .. }) if retries > 0 => {
                retries -= 1;
                continue;
            }
            Err(err) => return Err(err.into()),
            Ok(()) => {
                let _ = crate::run_quiet_async(tokio::process::Command::new("ifup").arg(&iface)).await;
                crate::profiles::reload_system().await?;
                break;
            }
        }
    }
    crate::activity::log(
        "satellite",
        "provision-satellite-tunnel",
        true,
        &format!("Provisioned satellite tunnel to Core for profile '{}'", p.profile),
        None,
    );
    Ok(())
}

// ── Load / persist ──────────────────────────────────────────────────────────

/// Read the persisted role, defaulting to Core when the marker is absent or
/// unreadable (a fresh, standalone router is a Core).
pub fn load_role() -> RoleMarker {
    std::fs::read_to_string(ROLE_PATH)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

fn load_registry() -> Vec<PairedSatellite> {
    std::fs::read_to_string(REGISTRY_PATH)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

async fn persist_json<T: Serialize>(path: &str, value: &T) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    let content = serde_json::to_string_pretty(value)
        .map_err(|e| Error::new(eyre!("serialize {path}: {e}"), ErrorKind::Serialization))?;
    let _ = tokio::fs::create_dir_all(DIR).await;
    let mut file = startos::util::io::AtomicFile::new(Path::new(path), None::<&Path>)
        .await
        .map_err(Error::from)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|e| Error::new(eyre!("chmod {path}: {e}"), ErrorKind::Filesystem))?;
    file.write_all(content.as_bytes())
        .await
        .map_err(|e| Error::new(eyre!("write {path}: {e}"), ErrorKind::Filesystem))?;
    file.save().await.map_err(Error::from)?;
    Ok(())
}

// ── Pure logic (unit-tested without touching disk) ──────────────────────────

fn parse_role(s: &str) -> Result<RouterRole, Error> {
    match s.trim().to_ascii_lowercase().as_str() {
        "core" => Ok(RouterRole::Core),
        "satellite" => Ok(RouterRole::Satellite),
        other => Err(Error::new(
            eyre!("invalid role '{other}' (expected 'core' or 'satellite')"),
            ErrorKind::InvalidRequest,
        )),
    }
}

/// Reject Core-only operations when running as a Satellite (a first slice of the
/// role-aware capability gating in the design's D7).
fn ensure_core() -> Result<(), Error> {
    if load_role().role == RouterRole::Satellite {
        return Err(Error::new(
            eyre!("this operation is only available on a Core router"),
            ErrorKind::Authorization,
        ));
    }
    Ok(())
}

fn ensure_satellite() -> Result<(), Error> {
    if load_role().role != RouterRole::Satellite {
        return Err(Error::new(
            eyre!("this operation is only available on a Satellite router"),
            ErrorKind::Authorization,
        ));
    }
    Ok(())
}

fn add_satellite(list: &mut Vec<PairedSatellite>, sat: PairedSatellite) -> Result<(), Error> {
    if list.iter().any(|s| s.name == sat.name) {
        return Err(Error::new(
            eyre!("a satellite named '{}' is already paired", sat.name),
            ErrorKind::Duplicate,
        ));
    }
    if list.iter().any(|s| s.public_key == sat.public_key) {
        return Err(Error::new(
            eyre!("that public key is already paired"),
            ErrorKind::Duplicate,
        ));
    }
    list.push(sat);
    Ok(())
}

fn remove_satellite(list: &mut Vec<PairedSatellite>, name: &str) -> bool {
    let before = list.len();
    list.retain(|s| s.name != name);
    list.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sat(name: &str, key: &str) -> PairedSatellite {
        PairedSatellite {
            name: name.to_string(),
            public_key: key.to_string(),
            mgmt_address: "10.0.0.2".to_string(),
            generation_applied: 0,
            last_seen: 0,
        }
    }

    #[test]
    fn role_defaults_to_core() {
        assert_eq!(RoleMarker::default().role, RouterRole::Core);
    }

    #[test]
    fn parse_role_accepts_known_roles() {
        assert_eq!(parse_role("core").unwrap(), RouterRole::Core);
        assert_eq!(parse_role("  Satellite ").unwrap(), RouterRole::Satellite);
        assert!(parse_role("gateway").is_err());
    }

    #[test]
    fn add_then_remove_satellite() {
        let mut list = Vec::new();
        add_satellite(&mut list, sat("s1", "KEY1")).unwrap();
        assert_eq!(list.len(), 1);
        assert!(remove_satellite(&mut list, "s1"));
        assert!(list.is_empty());
        assert!(!remove_satellite(&mut list, "s1"));
    }

    #[test]
    fn rejects_duplicate_name_and_key() {
        let mut list = Vec::new();
        add_satellite(&mut list, sat("s1", "KEY1")).unwrap();
        assert!(add_satellite(&mut list, sat("s1", "KEY2")).is_err());
        assert!(add_satellite(&mut list, sat("s2", "KEY1")).is_err());
        add_satellite(&mut list, sat("s2", "KEY2")).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn role_marker_round_trips() {
        let m = RoleMarker {
            role: RouterRole::Satellite,
            core_endpoint: Some("10.0.0.1:51820".to_string()),
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: RoleMarker = serde_json::from_str(&json).unwrap();
        assert_eq!(back.role, RouterRole::Satellite);
        assert_eq!(back.core_endpoint.as_deref(), Some("10.0.0.1:51820"));
    }

    #[test]
    fn allocate_ports_skips_used() {
        assert_eq!(
            allocate_listen_ports(&[51900, 51901], 3, 51900),
            vec![51902, 51903, 51904]
        );
    }

    #[test]
    fn allocate_ports_count_zero() {
        assert!(allocate_listen_ports(&[], 0, 51900).is_empty());
    }

    #[test]
    fn transit_addrs_are_disjoint_per_index() {
        let (c0, s0) = transit_addrs(0);
        let (c1, _s1) = transit_addrs(1);
        assert_eq!(c0.to_string(), "10.42.0.1");
        assert_eq!(s0.to_string(), "10.42.0.2");
        assert_eq!(c1.to_string(), "10.42.1.1");
        assert_ne!(c0, c1);
    }

    #[test]
    fn sync_snapshot_round_trips() {
        let snap = SyncSnapshot {
            generation: 7,
            ssid: "Home".into(),
            admin_key: "adminpw".into(),
            profiles: vec![ProfileSpec {
                interface: "guest".into(),
                vlan_tag: 101,
                subnet: "192.168.130.0/24".into(),
                wan_access: "all".into(),
                outbound: "wan".into(),
            }],
            passwords: vec![PasswordSpec {
                label: "Guest".into(),
                key: "guestpw".into(),
                vlan_tag: 101,
            }],
            ports: vec![PortSpec {
                port: "lan2".into(),
                vlan_tag: 101,
            }],
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: SyncSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
        assert!(json.contains("\"adminKey\""));
        assert!(json.contains("\"vlanTag\""));
    }
}

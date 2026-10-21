//! Router role, the main router's access-point registry, and the sync payload
//! (design `docs/design/satellite-router.md`, D25).

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::Path;

use clap::Parser;
use rpc_toolkit::{from_fn_async, from_fn_async_local, HandlerExt, ParentHandler};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::prelude::*;
use crate::utils::HandlerExtSerde;
use crate::{CliContext, CtrlContext, ServerContext};

const DIR: &str = "/etc/startwrt";
const ROLE_PATH: &str = "/etc/startwrt/role.json";
const REGISTRY_PATH: &str = "/etc/startwrt/access-points.json";

/// A router with no marker is a main router. An adopted router is an access
/// point of its main router.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default, visit_rs::VisitVariants,
)]
#[serde(rename_all = "camelCase")]
pub enum RouterRole {
    #[default]
    Main,
    AccessPoint,
}
rpc_toolkit::reflect_ts!(RouterRole);

/// Persisted role marker (`/etc/startwrt/role.json`).
#[derive(Clone, Debug, Serialize, Deserialize, Default, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct RoleMarker {
    pub role: RouterRole,
    /// On an access point, the main router's link-local endpoint with its
    /// interface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_endpoint: Option<String>,
    /// On an access point, the main router's WireGuard public key (base64)
    /// pinned at adoption. An access point with none waits to be adopted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_key: Option<String>,
    /// On an access point, the main router's address inside the management
    /// tunnel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_address: Option<String>,
    /// On an access point, the name the main router adopted it under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// On an access point, the port that carries the trunk to the main router.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uplink: Option<String>,
    /// On an access point, the sync generation applied last (0 = none).
    #[serde(default)]
    pub generation: u64,
}
rpc_toolkit::reflect_ts!(RoleMarker);

/// One access point the main router has adopted
/// (`/etc/startwrt/access-points.json`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct AccessPoint {
    /// Admin-facing label (unique).
    pub name: String,
    /// The access point's WireGuard public key (base64), pinned at adoption.
    pub public_key: String,
    /// The access point's address inside its management tunnel.
    pub mgmt_address: String,
    /// The config generation this access point last acknowledged (0 = never).
    #[serde(default)]
    pub generation_applied: u64,
    /// Unix seconds of last successful contact (0 = never).
    #[serde(default)]
    pub last_seen: i64,
    /// Where its tunnel last handshaked from, as `wg` prints it; the main
    /// router re-pins it after a reload so it can initiate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// The main router's trunk port its tunnel comes through, while it is live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    /// Its tunnel is receiving traffic (D18).
    #[serde(default)]
    pub live: bool,
    /// Access ports by name: the profile VLAN each carries untagged, or none.
    /// Set by the admin (`set-ports`); carried in every payload.
    #[serde(default)]
    pub ports: BTreeMap<String, Option<u16>>,
    /// What it reported last: its ports and radios.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<Inventory>,
    /// The devices on it, as it reported them last.
    #[serde(default)]
    pub clients: Vec<ClientInfo>,
    /// The main router's channel plan for its radios (D20).
    #[serde(default)]
    pub plan: BTreeMap<String, RadioPlan>,
    /// Its firmware version, as it reported it last.
    #[serde(default)]
    pub version: String,
    /// The recovery network it broadcasts while dark (D22), when it has a
    /// sticker password to protect it with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_ssid: Option<String>,
    /// It cannot read the main router's payload format and is dark until
    /// updated (D17).
    #[serde(default)]
    pub needs_update: bool,
    /// Its firmware update, from queued until it is back on the new version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<ApUpdate>,
    /// Ports it stopped forwarding on to break a loop.
    #[serde(default)]
    pub loop_blocked: Vec<String>,
}
rpc_toolkit::reflect_ts!(AccessPoint);

/// An access point's firmware update (D17).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct ApUpdate {
    /// The version it is updating to: the main router's.
    pub target: String,
    pub state: ApUpdateState,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
rpc_toolkit::reflect_ts!(ApUpdate);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, visit_rs::VisitVariants)]
#[serde(rename_all = "lowercase")]
pub enum ApUpdateState {
    Queued,
    Updating,
    Failed,
}
rpc_toolkit::reflect_ts!(ApUpdateState);

/// A device on an access point: which port or radio it is on, and its VLAN.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    /// Upper-case.
    pub mac: String,
    /// A port name, or a Wi-Fi band label such as `Wi-Fi 2.4GHz`.
    pub via: String,
    pub vlan: Option<u16>,
}
rpc_toolkit::reflect_ts!(ClientInfo);

/// An access point's hardware as it reports it (design §21: no assumed port
/// count; radios from the kernel, not from its wireless config).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub ports: Vec<PortInfo>,
    pub radios: Vec<RadioInfo>,
}
rpc_toolkit::reflect_ts!(Inventory);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct PortInfo {
    pub name: String,
    pub uplink: bool,
    pub link: bool,
}
rpc_toolkit::reflect_ts!(PortInfo);

/// A radio by its wireless-config name, with the bands its phy serves.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct RadioInfo {
    pub name: String,
    pub phy: String,
    /// `2g`, `5g`, `6g`.
    pub bands: Vec<String>,
}
rpc_toolkit::reflect_ts!(RadioInfo);

/// What an access point answers over the tunnel.
#[derive(Clone, Debug, Default, Serialize, Deserialize, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct TunnelStatus {
    /// The generation it applied last.
    pub generation: u64,
    pub tunnel_live: bool,
    pub dark: Option<bool>,
    pub inventory: Inventory,
    #[serde(default)]
    pub clients: Vec<ClientInfo>,
    /// Its firmware version.
    #[serde(default)]
    pub version: String,
    /// The recovery network it would broadcast while dark (D22).
    #[serde(default)]
    pub recovery_ssid: Option<String>,
    /// The newest payload format it reads; 0 predates the number and reads 1.
    #[serde(default)]
    pub format: u32,
    /// It cannot read the main router's payload format (D17).
    #[serde(default)]
    pub needs_update: bool,
    /// Its own firmware update, while one runs or after one failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<ApUpdate>,
    /// Ports it stopped forwarding on to break a loop.
    #[serde(default)]
    pub loop_blocked: Vec<String>,
    /// Its board, which picks the image the main router fetches for it.
    #[serde(default)]
    pub platform: String,
}
rpc_toolkit::reflect_ts!(TunnelStatus);

/// The payload format this firmware writes and the newest it reads (D17).
/// Raise it only when an access point that ignored a new field would serve
/// something the main router's settings forbid.
pub const SYNC_FORMAT: u32 = 1;

/// Serializes every read-modify-write of the registry across the daemon.
pub(crate) static REGISTRY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What an access point needs from its main router (D5). `generation` is
/// monotonic; an access point rejects a snapshot older than the one it applied.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct SyncSnapshot {
    pub generation: u64,
    /// `SYNC_FORMAT` of the main router that built it.
    #[serde(default)]
    pub format: u32,
    pub ssid: String,
    #[serde(default)]
    pub broadcast_separately: bool,
    /// Unset runs the world regulatory domain.
    pub country: Option<String>,
    /// The main router's channel plan for this access point, keyed by its radio
    /// name.
    pub radios: BTreeMap<String, RadioPlan>,
    pub profiles: Vec<ProfileSpec>,
    pub passwords: Vec<PasswordSpec>,
    /// Access ports by name: the profile VLAN each carries untagged, or none.
    pub ports: BTreeMap<String, Option<u16>>,
}
rpc_toolkit::reflect_ts!(SyncSnapshot);

/// A profile as its VLAN crosses the trunk.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSpec {
    pub fullname: String,
    pub interface: String,
    pub vlan_tag: u16,
}
rpc_toolkit::reflect_ts!(ProfileSpec);

/// One radio's assignment in the main router's channel plan.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct RadioPlan {
    pub band: String,
    pub channel: u32,
    /// OpenWrt `htmode`, such as `HE80`.
    pub htmode: String,
}
rpc_toolkit::reflect_ts!(RadioPlan);

/// One Wi-Fi password and the profile VLAN it lands on.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct PasswordSpec {
    pub label: String,
    pub key: String,
    pub vlan_tag: u16,
}
rpc_toolkit::reflect_ts!(PasswordSpec);

/// Management tunnel addresses: `10.42.<index>.0/24` per access point, disjoint
/// from every profile `/24`.
pub const TRANSIT_BASE: [u8; 2] = [10, 42];

/// The (main router, access point) addresses for tunnel `index`.
pub fn transit_addrs(index: u8) -> (Ipv4Addr, Ipv4Addr) {
    (
        Ipv4Addr::new(TRANSIT_BASE[0], TRANSIT_BASE[1], index, 1),
        Ipv4Addr::new(TRANSIT_BASE[0], TRANSIT_BASE[1], index, 2),
    )
}

// ── RPC surface ─────────────────────────────────────────────────────────────

pub fn access_point<C: CtrlContext>() -> ParentHandler<C> {
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
            "unpair",
            from_fn_async_local(unpair)
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
            "set-ports",
            from_fn_async_local(set_ports)
                .no_display()
                .with_call_remote::<CliContext>(),
        )
        .subcommand(
            "update",
            from_fn_async(update)
                .with_display_serializable()
                .with_call_remote::<CliContext>(),
        )
        .subcommand("adoption", crate::adoption::adoption::<C>())
}

#[derive(Deserialize, Serialize, Parser, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
pub struct UpdateParams {
    /// Access points to update; none means every one older than the main
    /// router.
    #[arg(long = "name")]
    #[serde(default)]
    names: Vec<String>,
}
rpc_toolkit::reflect_ts!(UpdateParams);

/// Queues access points to update to the main router's version, one at a time
/// (D17). Returns the names queued.
#[instrument(skip_all)]
pub async fn update(
    _ctx: ServerContext,
    UpdateParams { names }: UpdateParams,
) -> Result<Vec<String>, Error> {
    ensure_main()?;
    crate::fleet_update::start(names).await
}

#[instrument(skip_all)]
pub async fn get_role(_ctx: ServerContext) -> Result<RoleMarker, Error> {
    Ok(load_role())
}

#[derive(Deserialize, Serialize, Parser, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
pub struct SetRoleParams {
    /// "main" or "access-point".
    role: String,
}
rpc_toolkit::reflect_ts!(SetRoleParams);

/// Writes the marker alone; an access point set this way waits to be adopted.
#[instrument(skip_all)]
pub async fn set_role(
    _ctx: ServerContext,
    SetRoleParams { role }: SetRoleParams,
) -> Result<(), Error> {
    let role = parse_role(&role)?;
    persist_json(
        ROLE_PATH,
        &RoleMarker {
            role,
            ..Default::default()
        },
    )
    .await?;
    crate::activity::log(
        "access-point",
        "set-role",
        true,
        match role {
            RouterRole::Main => "Router role set to main router",
            RouterRole::AccessPoint => "Router role set to access point",
        },
        None,
    );
    Ok(())
}

#[instrument(skip_all)]
pub async fn list(_ctx: ServerContext) -> Result<Vec<AccessPoint>, Error> {
    ensure_main()?;
    Ok(load_registry())
}

#[derive(Deserialize, Serialize, Parser, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
pub struct UnpairParams {
    name: String,
}
rpc_toolkit::reflect_ts!(UnpairParams);

/// Forgets an access point: tells it so over the tunnel, then takes down the
/// main router's end. The access point goes back to waiting for a main router;
/// the trunk port closes when its tunnel stops.
#[instrument(skip_all)]
pub async fn unpair<C: CtrlContext>(
    ctx: C,
    UnpairParams { name }: UnpairParams,
) -> Result<(), Error> {
    ensure_main()?;
    let _guard = REGISTRY_LOCK.lock().await;
    let mut registry = load_registry();
    let Some(ap) = registry.iter().find(|s| s.name == name).cloned() else {
        return Err(Error::new(
            eyre!("no adopted access point named '{name}'"),
            ErrorKind::NotFound,
        ));
    };
    remove_access_point(&mut registry, &name);
    let released = if ctx.effectful() {
        match ap.mgmt_address.parse() {
            Ok(addr) => crate::control::release(addr).await.unwrap_or(false),
            Err(_) => false,
        }
    } else {
        false
    };
    {
        let arena = uciedit::Arena::new();
        let mut cfgs = uciedit::parse_all(ctx.uci_root(), &arena, &["network", "firewall"]).await?;
        crate::mgmt_tunnel::remove_main_end(&mut cfgs, &name)?;
        uciedit::dump_all(ctx.uci_root(), cfgs).await?;
    }
    persist_json(REGISTRY_PATH, &registry).await?;
    drop(_guard);
    if ctx.effectful() {
        let _ = crate::run_quiet_async(
            tokio::process::Command::new("/etc/init.d/network").arg("reload"),
        )
        .await;
        let _ = crate::run_quiet_async(
            tokio::process::Command::new("/etc/init.d/firewall").arg("reload"),
        )
        .await;
    }
    crate::activity::log(
        "access-point",
        "unpaired",
        true,
        &format!(
            "Removed access point '{name}'{}",
            if released {
                ""
            } else {
                " (it did not answer; it learns from its tunnel staying down)"
            }
        ),
        None,
    );
    Ok(())
}

#[derive(Deserialize, Serialize, Parser, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
pub struct SetPortsParams {
    name: String,
    /// Access port assignments as `<port>=<vlan>` or `<port>=off`.
    ports: Vec<String>,
}
rpc_toolkit::reflect_ts!(SetPortsParams);

fn parse_port_assignment(s: &str) -> Result<(String, Option<u16>), String> {
    let (port, vlan) = s
        .split_once('=')
        .ok_or_else(|| format!("expected <port>=<vlan|off>, got '{s}'"))?;
    let vlan = match vlan {
        "off" | "none" | "" => None,
        v => Some(v.parse().map_err(|_| format!("not a VLAN: '{v}'"))?),
    };
    Ok((port.to_string(), vlan))
}

/// Assigns an access point's ports to profiles. The next sync carries it.
#[instrument(skip_all)]
pub async fn set_ports(
    ctx: ServerContext,
    SetPortsParams { name, ports }: SetPortsParams,
) -> Result<(), Error> {
    ensure_main()?;
    let ports = ports
        .iter()
        .map(|p| parse_port_assignment(p))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|m| Error::new(eyre!("{m}"), ErrorKind::InvalidRequest))?;
    let tags: std::collections::BTreeSet<u16> = {
        let arena = uciedit::Arena::new();
        let cfgs = uciedit::parse_all(ctx.uci_root(), &arena, &["startwrt"]).await?;
        crate::profiles::list_config(ctx.clone(), &cfgs)?
            .into_iter()
            .map(|p| p.vlan_tag)
            .collect()
    };
    for (port, vlan) in &ports {
        if let Some(v) = vlan {
            if !tags.contains(v) {
                return Err(Error::new(
                    eyre!("port {port}: no profile has VLAN {v}"),
                    ErrorKind::InvalidRequest,
                ));
            }
        }
    }
    let _guard = REGISTRY_LOCK.lock().await;
    let mut registry = load_registry();
    let ap = registry
        .iter_mut()
        .find(|s| s.name == name)
        .ok_or_else(|| {
            Error::new(
                eyre!("no adopted access point named '{name}'"),
                ErrorKind::NotFound,
            )
        })?;
    if let Some(inv) = &ap.inventory {
        for (port, _) in &ports {
            match inv.ports.iter().find(|p| p.name == *port) {
                None => {
                    return Err(Error::new(
                        eyre!("'{name}' has no port named {port}"),
                        ErrorKind::InvalidRequest,
                    ))
                }
                Some(p) if p.uplink => {
                    return Err(Error::new(
                        eyre!("{port} is the uplink of '{name}'"),
                        ErrorKind::InvalidRequest,
                    ))
                }
                Some(_) => {}
            }
        }
    }
    ap.ports = ports.into_iter().collect();
    persist_registry(&registry).await?;
    crate::activity::log(
        "access-point",
        "set-ports",
        true,
        &format!("Updated the port assignments of access point '{name}'"),
        None,
    );
    Ok(())
}

#[derive(Debug, Serialize, Deserialize, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct StatusResponse {
    pub role: RouterRole,
    pub main_endpoint: Option<String>,
    pub uplink: Option<String>,
    pub generation: u64,
    /// On an access point: its management tunnel is receiving traffic.
    pub tunnel_live: Option<bool>,
    /// On an access point: SSIDs down and access ports cut (D18); unset
    /// before the first reading.
    pub dark: Option<bool>,
    pub access_point_count: usize,
    pub access_points: Vec<AccessPoint>,
}
rpc_toolkit::reflect_ts!(StatusResponse);

#[instrument(skip_all)]
pub async fn status(_ctx: ServerContext) -> Result<StatusResponse, Error> {
    let marker = load_role();
    let access_points = if marker.role == RouterRole::Main {
        load_registry()
    } else {
        Vec::new()
    };
    let (tunnel_live, dark) = if marker.role == RouterRole::AccessPoint && marker.main_key.is_some()
    {
        let (live, dark) = crate::liveness::ap_status();
        (Some(live), dark)
    } else {
        (None, None)
    };
    Ok(StatusResponse {
        role: marker.role,
        main_endpoint: marker.main_endpoint,
        uplink: marker.uplink,
        generation: marker.generation,
        tunnel_live,
        dark,
        access_point_count: access_points.len(),
        access_points,
    })
}

// ── Load / persist ──────────────────────────────────────────────────────────

/// The persisted role; a missing or unreadable marker reads as a main router.
pub fn load_role() -> RoleMarker {
    std::fs::read_to_string(ROLE_PATH)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

pub(crate) fn load_registry() -> Vec<AccessPoint> {
    std::fs::read_to_string(REGISTRY_PATH)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

pub(crate) async fn persist_registry(list: &[AccessPoint]) -> Result<(), Error> {
    persist_json(REGISTRY_PATH, &list).await
}

pub(crate) async fn persist_role(marker: &RoleMarker) -> Result<(), Error> {
    persist_json(ROLE_PATH, marker).await
}

pub(crate) async fn persist_generation(generation: u64) -> Result<(), Error> {
    let mut role = load_role();
    role.generation = generation;
    persist_role(&role).await
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

// ── Pure logic ──────────────────────────────────────────────────────────────

fn parse_role(s: &str) -> Result<RouterRole, Error> {
    match s.trim().to_ascii_lowercase().replace('-', "").as_str() {
        "main" => Ok(RouterRole::Main),
        "accesspoint" => Ok(RouterRole::AccessPoint),
        _ => Err(Error::new(
            eyre!(
                "invalid role '{}' (expected 'main' or 'access-point')",
                s.trim()
            ),
            ErrorKind::InvalidRequest,
        )),
    }
}

/// A first slice of D7: main-router-only operations refuse on an access point.
pub(crate) fn ensure_main() -> Result<(), Error> {
    if load_role().role == RouterRole::AccessPoint {
        return Err(Error::new(
            eyre!("this operation is only available on a main router"),
            ErrorKind::Authorization,
        ));
    }
    Ok(())
}

pub(crate) fn add_access_point(list: &mut Vec<AccessPoint>, ap: AccessPoint) -> Result<(), Error> {
    if list.iter().any(|s| s.name == ap.name) {
        return Err(Error::new(
            eyre!("an access point named '{}' is already adopted", ap.name),
            ErrorKind::Duplicate,
        ));
    }
    if list.iter().any(|s| s.public_key == ap.public_key) {
        return Err(Error::new(
            eyre!("that public key is already adopted"),
            ErrorKind::Duplicate,
        ));
    }
    list.push(ap);
    Ok(())
}

pub(crate) fn remove_access_point(list: &mut Vec<AccessPoint>, name: &str) -> bool {
    let before = list.len();
    list.retain(|s| s.name != name);
    list.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ap(name: &str, key: &str) -> AccessPoint {
        AccessPoint {
            name: name.to_string(),
            public_key: key.to_string(),
            mgmt_address: "10.42.1.2".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn role_defaults_to_main() {
        assert_eq!(RouterRole::default(), RouterRole::Main);
        assert_eq!(RoleMarker::default().role, RouterRole::Main);
    }

    #[test]
    fn parse_role_accepts_known_roles() {
        assert_eq!(parse_role("main").unwrap(), RouterRole::Main);
        assert_eq!(
            parse_role(" Access-Point ").unwrap(),
            RouterRole::AccessPoint
        );
        assert_eq!(parse_role("accessPoint").unwrap(), RouterRole::AccessPoint);
        assert!(parse_role("core").is_err());
        assert!(parse_role("hub").is_err());
    }

    #[test]
    fn add_then_remove_access_point() {
        let mut list = Vec::new();
        add_access_point(&mut list, ap("a", "k1")).unwrap();
        add_access_point(&mut list, ap("b", "k2")).unwrap();
        assert_eq!(list.len(), 2);
        assert!(remove_access_point(&mut list, "a"));
        assert!(!remove_access_point(&mut list, "a"));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn rejects_duplicate_name_and_key() {
        let mut list = Vec::new();
        add_access_point(&mut list, ap("a", "k1")).unwrap();
        assert!(add_access_point(&mut list, ap("a", "k2")).is_err());
        assert!(add_access_point(&mut list, ap("b", "k1")).is_err());
    }

    #[test]
    fn role_marker_round_trips() {
        let marker = RoleMarker {
            role: RouterRole::AccessPoint,
            main_endpoint: Some("fe80::1%eth1".into()),
            main_key: Some("KEY=".into()),
            main_address: Some("10.42.1.1".into()),
            name: Some("garage".into()),
            uplink: Some("eth1".into()),
            generation: 4,
        };
        let json = serde_json::to_string(&marker).unwrap();
        assert!(json.contains(r#""role":"accessPoint""#));
        let back: RoleMarker = serde_json::from_str(&json).unwrap();
        assert_eq!(back.role, RouterRole::AccessPoint);
        assert_eq!(back.uplink.as_deref(), Some("eth1"));
        assert_eq!(back.generation, 4);
        let old: RoleMarker = serde_json::from_str(r#"{"role":"accessPoint"}"#).unwrap();
        assert_eq!(old.generation, 0);
        assert!(old.uplink.is_none());
    }

    #[test]
    fn port_assignments_parse() {
        assert_eq!(
            parse_port_assignment("eth0=101").unwrap(),
            ("eth0".to_string(), Some(101))
        );
        assert_eq!(
            parse_port_assignment("eth2=off").unwrap(),
            ("eth2".to_string(), None)
        );
        assert!(parse_port_assignment("eth0").is_err());
        assert!(parse_port_assignment("eth0=guest").is_err());
    }

    #[test]
    fn transit_addrs_are_disjoint_per_index() {
        let (c1, s1) = transit_addrs(1);
        let (c2, s2) = transit_addrs(2);
        assert_ne!(c1, c2);
        assert_ne!(s1, s2);
        assert_eq!(c1.octets()[3], 1);
        assert_eq!(s1.octets()[3], 2);
    }

    #[test]
    fn sync_snapshot_round_trips() {
        let snap = SyncSnapshot {
            generation: 7,
            format: SYNC_FORMAT,
            ssid: "Home".into(),
            broadcast_separately: true,
            country: Some("US".into()),
            radios: BTreeMap::from([(
                "default_radio0".into(),
                RadioPlan {
                    band: "2g".into(),
                    channel: 6,
                    htmode: "HE20".into(),
                },
            )]),
            profiles: vec![ProfileSpec {
                fullname: "guest".into(),
                interface: "yyiph".into(),
                vlan_tag: 101,
            }],
            passwords: vec![PasswordSpec {
                label: "Guest".into(),
                key: "guestpassword".into(),
                vlan_tag: 101,
            }],
            ports: BTreeMap::from([("eth0".into(), Some(101)), ("eth2".into(), None)]),
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("\"vlanTag\":101"));
        assert_eq!(serde_json::from_str::<SyncSnapshot>(&json).unwrap(), snap);
    }
}

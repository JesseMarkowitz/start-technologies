//! Wired 802.1X network authentication.
//!
//! StartWRT routers take one of two roles in an 802.1X network (see the design
//! doc `docs/design/wired-dot1x.md`):
//!
//! - **Core** — the single authority: holds the on-device RADIUS server, owns the
//!   security profiles, and is the authenticator on its own LAN ports.
//! - **Satellite** — extends wired/wireless coverage; its LAN ports authenticate
//!   wired clients and forward those requests upstream toward the Core over a
//!   trusted 802.1Q trunk (Model B, §3d).
//!
//! This module owns the `dot1x.*` RPC surface (`get`/`set`/`status`/`logs`) and the
//! derivation of the RADIUS user database from profile passwords (§4b). Persistence
//! lives in UCI: global settings in `config dot1x`, per-port modes in
//! `config dot1x_port` (both in `/etc/config/startwrt`), and the RADIUS server in
//! `/etc/config/radius`. The generated `/etc/radius/users` file is 100% derived and
//! is regenerated on `dot1x.set` / at boot — never backed up (§5).
//!
//! Per-port authentication mode is also surfaced on `ethernet::Port::auth_mode`; the
//! reconciliation of `ethernet.set` with the `dot1x_port` sections and the
//! per-port hostapd-wired authenticator config are handled in `ethernet.rs`.

use crate::invoke::Invoke;
use crate::prelude::*;
use crate::profiles::{self, ProfileId};
use crate::utils::{DeserializeStdin, HandlerExtSerde};
use crate::CtrlContext;
use rpc_toolkit::{from_fn_async_local, ParentHandler};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uciedit::openwrt::{Dot1xGlobal, Dot1xPort, RadiusConfig, WifiStation};
use uciedit::{dump_all, parse_all, Arena, Configs};

/// `/etc/radius/` — home for the generated, non-UCI RADIUS databases (§5). Both
/// files hold plaintext secrets and are written mode 0600.
pub const RADIUS_DIR: &str = "/etc/radius";
/// Generated JSON user DB, derived from profile passwords by
/// [`build_radius_users_json`]. Regenerated on `dot1x.set` and at boot.
pub const RADIUS_USERS_PATH: &str = "/etc/radius/users";
/// `<CIDR> <secret>` client (authenticator) list — one entry per paired Satellite.
/// Generated from the `satellite` registry (pairing, §5a — not yet implemented).
pub const RADIUS_CLIENTS_PATH: &str = "/etc/radius/clients";

// ── Data model ──────────────────────────────────────────────────────────────

/// The role a StartWRT router plays in an 802.1X network. There is exactly one
/// Core per network; every other router is a Satellite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Dot1xRole {
    /// Holds the RADIUS server, owns the profiles, authenticator on its LAN ports.
    Core,
    /// Authenticator on its LAN ports; forwards auth upstream toward the Core.
    Satellite,
}

/// Per-LAN-port authentication mode.
///
/// Generic over the profile-id representation `Id` so it round-trips through the
/// same id mapping as the sibling `ethernet::Port::profile` field (defaults to
/// [`ProfileId`]; the `ethernet.set` request instantiates it with `ProfileIdOpt`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum PortAuthMode<Id = ProfileId> {
    /// Fixed profile assignment — today's default. The profile comes from the
    /// sibling `Port::profile` field; every device on the port lands there.
    Static,
    /// Per-device 802.1X authentication. An authenticated device gets its
    /// credential-derived profile; an unauthenticated/failed device falls back to
    /// `guest` (a required, explicit choice — no silent inheritance).
    Dot1xClient { guest: Id },
    /// A trusted, statically-configured 802.1Q trunk to a paired Satellite.
    SatelliteUplink,
}

impl<Id> Default for PortAuthMode<Id> {
    fn default() -> Self {
        PortAuthMode::Static
    }
}

/// serde `default` for the `ethernet::Port::auth_mode` field. Named (rather than a
/// bare `#[serde(default)]`) so serde calls this function — inferring `Id` from the
/// return type — instead of adding an `Id: Default` bound to `Port`'s generated
/// `Deserialize` impl. `Port` is instantiated with both `ProfileId` and
/// `ProfileIdOpt`, neither of which is `Default`, so the bound would not hold.
pub fn default_auth_mode<Id>() -> PortAuthMode<Id> {
    PortAuthMode::Static
}

/// Satellite-only, read-only upstream identity + Core reachability, provisioned by
/// pairing (§5a) rather than entered by the admin. Absent on a Core or an unpaired
/// device.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamInfo {
    /// The 802.1X identity this Satellite presents upstream.
    pub identity: String,
    /// Address at which this Satellite reaches the Core's management surface.
    pub core_mgmt_addr: String,
    /// Management VLAN id carrying Core↔Satellite control traffic.
    pub mgmt_vlan: u16,
}

/// The full 802.1X configuration for a router, as read from / written to UCI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dot1xConfig {
    /// Whether wired 802.1X is enabled on this router at all.
    pub enabled: bool,
    /// Core or Satellite.
    pub role: Dot1xRole,
    /// Present only on a paired Satellite (see [`UpstreamInfo`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<UpstreamInfo>,
    /// Per LAN port: authentication mode. The guest profile for a `Dot1xClient`
    /// port is carried inside the [`PortAuthMode`] variant.
    #[serde(default)]
    pub ports: BTreeMap<String, PortAuthMode>,
}

/// Runtime authentication state, reported by `dot1x.status` from hostapd via ubus.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dot1xStatus {
    pub enabled: bool,
    pub role: Dot1xRole,
    pub ports: Vec<PortStatus>,
}

/// Per-`Dot1xClient`-port runtime state.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortStatus {
    pub port: String,
    /// Whether the hostapd authenticator ubus object for this port is present.
    pub present: bool,
    /// MACs currently authenticated on this port (from hostapd `get_clients`).
    pub clients: Vec<String>,
}

// ── Handler ─────────────────────────────────────────────────────────────────

pub fn dot1x<C: CtrlContext + Clone>() -> ParentHandler<C> {
    ParentHandler::new()
        .subcommand("get", from_fn_async_local(get::<C>).with_display_serializable())
        .subcommand("set", from_fn_async_local(set::<C>).with_display_serializable())
        .subcommand("status", from_fn_async_local(status::<C>).with_display_serializable())
        .subcommand("logs", from_fn_async_local(logs::<C>).with_display_serializable())
}

// ── get ─────────────────────────────────────────────────────────────────────

pub async fn get<C: CtrlContext>(ctx: C) -> Result<Dot1xConfig, Error> {
    let arena = Arena::new();
    let cfgs = parse_all(ctx.uci_root(), &arena, &["startwrt"]).await?;
    get_config(&ctx, &cfgs)
}

fn get_config(ctx: &impl CtrlContext, cfgs: &Configs) -> Result<Dot1xConfig, Error> {
    let lookup = profiles::Lookup::parse(ctx.clone(), cfgs)?;
    // Absent `dot1x` section ⇒ un-provisioned router ⇒ 802.1X off. (We can't use
    // `unwrap_or_default()`: `Dot1xGlobal::default()` gives `disabled = false` — the
    // `#[uci(default_value = true)]` only applies when *reading* a present section
    // that omits the field, not to Rust's `Default`.)
    let global = cfgs["startwrt"]
        .sections
        .iter()
        .find(|s| s.ty() == "dot1x")
        .map(|s| s.get::<Dot1xGlobal>())
        .transpose()?;
    let (enabled, role) = match &global {
        Some(g) => (!g.disabled, parse_role(&g.role)),
        None => (false, Dot1xRole::Core),
    };
    let upstream = match (role, global.as_ref().and_then(|g| g.upstream_identity.clone())) {
        (Dot1xRole::Satellite, Some(identity)) => {
            let g = global.as_ref();
            Some(UpstreamInfo {
                identity,
                core_mgmt_addr: g.and_then(|g| g.core_mgmt_addr.clone()).unwrap_or_default(),
                mgmt_vlan: g.and_then(|g| g.mgmt_vlan).unwrap_or(0),
            })
        }
        _ => None,
    };
    let mut ports = BTreeMap::new();
    cfgs["startwrt"].try_each(|_, p: Dot1xPort| {
        let mode = port_mode_from_uci(&p, &lookup)?;
        ports.insert(p.port, mode);
        Ok::<_, Error>(())
    })?;
    Ok(Dot1xConfig {
        enabled,
        role,
        upstream,
        ports,
    })
}

fn parse_role(s: &str) -> Dot1xRole {
    match s {
        "satellite" => Dot1xRole::Satellite,
        _ => Dot1xRole::Core,
    }
}

fn role_to_uci(r: Dot1xRole) -> &'static str {
    match r {
        Dot1xRole::Core => "core",
        Dot1xRole::Satellite => "satellite",
    }
}

/// Resolve a persisted `dot1x_port` section into a `PortAuthMode`, mapping the
/// stored guest VLAN back to a profile via `Lookup`. Shared with `ethernet::get`,
/// which overlays these modes onto its `Port` list.
pub(crate) fn port_mode_from_uci(
    p: &Dot1xPort,
    lookup: &profiles::Lookup,
) -> Result<PortAuthMode, Error> {
    Ok(match p.mode.as_str() {
        "dot1xClient" => {
            let vlan = p.guest_vlan.ok_or_else(|| {
                Error::new(
                    eyre!("dot1x port '{}' is dot1xClient but has no guest_vlan", p.port),
                    ErrorKind::InvalidRequest,
                )
            })?;
            let guest = lookup.from_vlan(vlan).cloned().ok_or_else(|| {
                Error::new(
                    eyre!("dot1x port '{}' guest VLAN {vlan} matches no profile", p.port),
                    ErrorKind::MissingProfile,
                )
            })?;
            PortAuthMode::Dot1xClient { guest }
        }
        "satelliteUplink" => PortAuthMode::SatelliteUplink,
        _ => PortAuthMode::Static,
    })
}

// ── set ─────────────────────────────────────────────────────────────────────

pub async fn set<C: CtrlContext>(
    ctx: C,
    DeserializeStdin(req): DeserializeStdin<Dot1xConfig>,
) -> Result<Dot1xConfig, Error> {
    let mut retries = 4;
    loop {
        let arena = Arena::new();
        let mut cfgs =
            parse_all(ctx.uci_root(), &arena, &["startwrt", "wireless", "radius"]).await?;
        let lookup = profiles::Lookup::parse(ctx.clone(), &cfgs)?;
        if let Err(err) = write_config(&mut cfgs, &req, &lookup) {
            crate::activity::log(
                "dot1x",
                "updated",
                false,
                "Failed to update 802.1X configuration",
                Some(&err.to_string()),
            );
            return Err(err);
        }
        match dump_all(ctx.uci_root(), cfgs).await {
            Err(uciedit::Error::Conflict { .. }) if retries > 0 => {
                retries -= 1;
                continue;
            }
            Err(err) => {
                crate::activity::log(
                    "dot1x",
                    "updated",
                    false,
                    "Failed to update 802.1X configuration",
                    Some(&err.to_string()),
                );
                return Err(err.into());
            }
            Ok(()) => break,
        }
    }

    // The user DB is a derived, non-UCI artifact (§5): regenerate it whenever this
    // router is an enabled Core. Guarded by `effectful()` so `--configs-only` stays
    // pure and tests never touch the real `/etc/radius/`.
    if ctx.effectful() && req.enabled && req.role == Dot1xRole::Core {
        write_radius_users(&ctx).await?;
    }
    if ctx.effectful() {
        reload_services(&req).await;
    }
    crate::activity::log("dot1x", "updated", true, "Updated 802.1X configuration", None);
    get(ctx).await
}

/// Rewrite the `dot1x`/`dot1x_port` sections in `startwrt` and the enable bit on the
/// `radius` server section from `req`. Fully replaces the prior dot1x sections so a
/// removed port drops out cleanly.
fn write_config(
    cfgs: &mut Configs,
    req: &Dot1xConfig,
    lookup: &profiles::Lookup,
) -> Result<(), Error> {
    cfgs["startwrt"]
        .sections
        .retain(|s| s.ty() != "dot1x" && s.ty() != "dot1x_port");

    let (upstream_identity, core_mgmt_addr, mgmt_vlan) = match (req.role, &req.upstream) {
        (Dot1xRole::Satellite, Some(u)) => (
            Some(u.identity.clone()),
            Some(u.core_mgmt_addr.clone()),
            Some(u.mgmt_vlan),
        ),
        _ => (None, None, None),
    };
    cfgs["startwrt"].append(
        &Dot1xGlobal {
            disabled: !req.enabled,
            role: role_to_uci(req.role).to_string(),
            upstream_identity,
            core_mgmt_addr,
            mgmt_vlan,
        },
        Some("dot1x"),
    )?;

    for (port, mode) in &req.ports {
        let (mode_str, guest_vlan) = match mode {
            PortAuthMode::Static => ("static", None),
            PortAuthMode::Dot1xClient { guest } => {
                // A `Dot1xClient` port must name a real guest profile — reject the
                // whole write otherwise (no silent fallback, §4c).
                lookup.resolve(&guest.clone().into())?;
                ("dot1xClient", Some(guest.vlan_tag))
            }
            PortAuthMode::SatelliteUplink => ("satelliteUplink", None),
        };
        cfgs["startwrt"].append(
            &Dot1xPort {
                port: port.clone(),
                mode: mode_str.to_string(),
                guest_vlan,
            },
            Some(port),
        )?;
    }

    apply_radius(cfgs, req)?;
    Ok(())
}

/// Flip the on-device RADIUS server on (enabled Core) or off, preserving a
/// pre-staged `radius` section's fields and synthesizing a full one (pointed at the
/// StartWRT certs) if none is present.
fn apply_radius(cfgs: &mut Configs, req: &Dot1xConfig) -> Result<(), Error> {
    let want_radius = req.enabled && req.role == Dot1xRole::Core;
    // A partially-staged/absent section is treated as "rebuild".
    let existing = cfgs["radius"]
        .sections
        .iter()
        .find(|s| s.ty() == "radius")
        .and_then(|s| s.get::<RadiusConfig>().ok());
    let radius = match existing {
        Some(mut r) => {
            r.disabled = !want_radius;
            r
        }
        None => RadiusConfig {
            disabled: !want_radius,
            ipv6: None,
            log_level: None,
            ca_cert: crate::ssl::ca_cert_path().to_string_lossy().into_owned(),
            cert: crate::ssl::server_cert_path().to_string_lossy().into_owned(),
            key: crate::ssl::server_key_path().to_string_lossy().into_owned(),
            users: RADIUS_USERS_PATH.to_string(),
            clients: RADIUS_CLIENTS_PATH.to_string(),
            auth_port: None,
            acct_port: None,
            identity: None,
        },
    };
    cfgs["radius"].sections.retain(|s| s.ty() != "radius");
    cfgs["radius"].append(&radius, Some("radius"))?;
    Ok(())
}

// ── RADIUS user DB derivation (§4b) ──────────────────────────────────────────

/// Build the `/etc/radius/users` JSON DB from the same `WifiStation` set the Wi-Fi
/// path uses (§4b). Each profile-bound password becomes one PEAP/phase2 user:
/// username = station label (defaults to the profile fullname), password = the
/// station key, `vlan-id` = the profile's VLAN tag. Admin passwords (no `vid`, i.e.
/// no profile) are excluded — they are not network-access credentials.
///
/// Known limitation (open question in §4b): two passwords sharing a label collide on
/// the username; last write wins. The frontend enforces unique labels today.
pub fn build_radius_users_json(ctx: &impl CtrlContext, cfgs: &Configs) -> Result<String, Error> {
    let lookup = profiles::Lookup::parse(ctx.clone(), cfgs)?;
    let mut users = serde_json::Map::new();
    cfgs["wireless"].try_each(|_, station: WifiStation| {
        // No VLAN ⇒ admin password ⇒ not a network credential.
        let Some(vid) = station.vid else {
            return Ok(());
        };
        let Some(profile) = lookup.from_vlan(vid).cloned() else {
            return Ok(());
        };
        let username = station
            .label
            .clone()
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| profile.fullname.clone());
        users.insert(
            username,
            serde_json::json!({
                "password": station.key,
                "methods": ["MSCHAPV2"],
                "vlan-id": profile.vlan_tag,
            }),
        );
        Ok::<_, Error>(())
    })?;
    let doc = serde_json::json!({
        "phase1": { "wildcard": [ { "name": "*", "methods": ["PEAP"] } ] },
        "phase2": { "users": users },
    });
    serde_json::to_string_pretty(&doc)
        .map_err(|e| Error::new(eyre!("serialize radius users: {e}"), ErrorKind::Serialization))
}

/// Regenerate `/etc/radius/users` atomically (temp + rename, mode 0600) from the
/// current UCI. Cheap; called from `dot1x.set` and (later) profile mutations and the
/// boot path. Effectful — writes a fixed system path, so callers must gate on
/// `effectful()`.
pub async fn write_radius_users(ctx: &impl CtrlContext) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncWriteExt;

    let arena = Arena::new();
    let cfgs = parse_all(ctx.uci_root(), &arena, &["startwrt", "wireless"]).await?;
    let content = build_radius_users_json(ctx, &cfgs)?;

    let _ = tokio::fs::create_dir_all(RADIUS_DIR).await;
    let mut file =
        startos::util::io::AtomicFile::new(std::path::Path::new(RADIUS_USERS_PATH), None::<&std::path::Path>)
            .await
            .map_err(Error::from)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|e| Error::new(eyre!("chmod radius users: {e}"), ErrorKind::Filesystem))?;
    file.write_all(content.as_bytes())
        .await
        .map_err(|e| Error::new(eyre!("write radius users: {e}"), ErrorKind::Filesystem))?;
    file.save().await.map_err(Error::from)?;
    Ok(())
}

/// Regenerate the RADIUS user DB **iff** this router is an enabled Core; a no-op
/// otherwise. Safe to call from any profile mutation (`profiles.create/set/delete`,
/// §4b) so `/etc/radius/users` tracks profile-password changes. Effectful (writes a
/// fixed system path) — callers must gate on `effectful()`.
pub async fn maybe_regenerate_radius_users(ctx: &impl CtrlContext) -> Result<(), Error> {
    let cfg = get(ctx.clone()).await?;
    if cfg.enabled && cfg.role == Dot1xRole::Core {
        write_radius_users(ctx).await?;
    }
    Ok(())
}

// ── status / logs ────────────────────────────────────────────────────────────

pub async fn status<C: CtrlContext>(ctx: C) -> Result<Dot1xStatus, Error> {
    let arena = Arena::new();
    let cfgs = parse_all(ctx.uci_root(), &arena, &["startwrt"]).await?;
    let cfg = get_config(&ctx, &cfgs)?;
    let mut ports = Vec::new();
    for (port, mode) in &cfg.ports {
        if !matches!(mode, PortAuthMode::Dot1xClient { .. }) {
            continue;
        }
        let obj = format!("hostapd.{port}");
        let out = run_ubus(&["call", &obj, "get_clients"]).await;
        ports.push(PortStatus {
            port: port.clone(),
            present: !out.is_empty(),
            clients: parse_client_macs(&out),
        });
    }
    Ok(Dot1xStatus {
        enabled: cfg.enabled,
        role: cfg.role,
        ports,
    })
}

/// Return recent hostapd/RADIUS log lines so an admin can diagnose auth failures
/// without SSH (§4a). Backed by `logread` filtered to the hostapd/RADIUS sources.
pub async fn logs<C: CtrlContext>(_ctx: C) -> Result<crate::logs::LogsResponse, Error> {
    let out = tokio::process::Command::new("logread")
        .invoke(ErrorKind::Filesystem.into())
        .await?;
    let text = String::from_utf8_lossy(&out);
    let entries = text
        .lines()
        .filter(|l| l.contains("hostapd") || l.contains("radius") || l.contains("RADIUS"))
        .filter_map(crate::logs::parse_logread_line)
        .collect();
    Ok(crate::logs::LogsResponse { entries })
}

async fn run_ubus(args: &[&str]) -> String {
    tokio::process::Command::new("ubus")
        .args(args)
        .invoke(ErrorKind::Network.into())
        .await
        .ok()
        .and_then(|out| String::from_utf8(out).ok())
        .unwrap_or_default()
}

/// Extract the client MAC keys from a hostapd `get_clients` ubus response:
/// `{ "clients": { "<MAC>": { ... }, ... } }`.
fn parse_client_macs(ubus_out: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(ubus_out)
        .ok()
        .and_then(|v| {
            v.get("clients")
                .and_then(|c| c.as_object())
                .map(|m| m.keys().cloned().collect())
        })
        .unwrap_or_default()
}

async fn reload_services(req: &Dot1xConfig) {
    // Core: (re)start the RADIUS server so a new user DB / enable bit takes effect.
    if req.role == Dot1xRole::Core {
        let _ = crate::run_quiet_async(
            tokio::process::Command::new("/etc/init.d/radius").arg("reload"),
        )
        .await;
    }
    // Re-run hostapd so per-port authenticator configs are (re)applied.
    let _ = crate::run_quiet_async(&mut tokio::process::Command::new("wifi")).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpc_toolkit::Context;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::runtime::Runtime;

    #[derive(Clone)]
    struct TestContext(PathBuf);

    impl Context for TestContext {
        fn runtime(&self) -> Option<Arc<Runtime>> {
            None
        }
    }

    impl CtrlContext for TestContext {
        fn uci_root(&self) -> PathBuf {
            self.0.clone()
        }
        fn effectful(&self) -> bool {
            false
        }
    }

    /// Two profiles (Admin lan/vlan 99, Guest guest/vlan 101), plus a `wireless`
    /// file with one admin password (no vid) and one Guest-bound password.
    fn setup_configs(dir: &std::path::Path) {
        std::fs::write(
            dir.join("startwrt"),
            "\
config profile lan
\toption fullname 'Admin'
\toption interface 'lan'
\toption vlan_tag '99'

config profile guest
\toption fullname 'Guest'
\toption interface 'guest'
\toption vlan_tag '101'
",
        )
        .unwrap();
        std::fs::write(
            dir.join("wireless"),
            "\
config wifi-station
\toption key 'adminpass'

config wifi-station
\toption key 'guestpass'
\toption vid '101'
\toption label 'Guest'
",
        )
        .unwrap();
        std::fs::write(dir.join("radius"), "").unwrap();
    }

    #[tokio::test]
    async fn radius_users_derives_from_profile_passwords() {
        let dir = tempfile::tempdir().unwrap();
        setup_configs(dir.path());
        let ctx = TestContext(dir.path().to_path_buf());
        let arena = Arena::new();
        let cfgs = parse_all(ctx.uci_root(), &arena, &["startwrt", "wireless"])
            .await
            .unwrap();
        let json = build_radius_users_json(&ctx, &cfgs).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let users = v["phase2"]["users"].as_object().unwrap();
        // Admin password (no vid) excluded; Guest password present, keyed by label.
        assert_eq!(users.len(), 1, "only the profile-bound password becomes a user");
        let guest = &users["Guest"];
        assert_eq!(guest["password"], "guestpass");
        assert_eq!(guest["vlan-id"], 101);
        assert_eq!(guest["methods"][0], "MSCHAPV2");
        assert_eq!(v["phase1"]["wildcard"][0]["methods"][0], "PEAP");
    }

    #[tokio::test]
    async fn get_defaults_to_disabled_when_no_dot1x_section() {
        // An un-provisioned router (no `config dot1x` section) must report 802.1X
        // OFF — regression guard against `unwrap_or_default()` reading `disabled`
        // as Rust's `false`.
        let dir = tempfile::tempdir().unwrap();
        setup_configs(dir.path());
        let ctx = TestContext(dir.path().to_path_buf());
        let cfg = get(ctx).await.unwrap();
        assert!(!cfg.enabled, "no dot1x section ⇒ disabled");
        assert_eq!(cfg.role, Dot1xRole::Core);
        assert!(cfg.ports.is_empty());
    }

    #[tokio::test]
    async fn set_then_get_round_trips_config() {
        let dir = tempfile::tempdir().unwrap();
        setup_configs(dir.path());
        let ctx = TestContext(dir.path().to_path_buf());

        let guest_profile = ProfileId {
            fullname: "Guest".into(),
            interface: "guest".into(),
            vlan_tag: 101,
        };
        let mut ports = BTreeMap::new();
        ports.insert("eth0".to_string(), PortAuthMode::Static);
        ports.insert(
            "eth1".to_string(),
            PortAuthMode::Dot1xClient {
                guest: guest_profile,
            },
        );
        let req = Dot1xConfig {
            enabled: true,
            role: Dot1xRole::Core,
            upstream: None,
            ports,
        };

        let got = set(ctx.clone(), DeserializeStdin(req)).await.unwrap();
        assert!(got.enabled);
        assert_eq!(got.role, Dot1xRole::Core);
        assert!(matches!(got.ports["eth0"], PortAuthMode::Static));
        assert!(matches!(
            got.ports["eth1"],
            PortAuthMode::Dot1xClient { .. }
        ));

        // The RADIUS server section flipped enabled (disabled = 0) on a Core.
        let arena = Arena::new();
        let cfgs = parse_all(ctx.uci_root(), &arena, &["radius"])
            .await
            .unwrap();
        let radius = cfgs["radius"]
            .sections
            .iter()
            .find(|s| s.ty() == "radius")
            .unwrap()
            .get::<RadiusConfig>()
            .unwrap();
        assert!(!radius.disabled);
        assert_eq!(radius.users, RADIUS_USERS_PATH);
    }

    #[tokio::test]
    async fn set_rejects_unknown_guest_profile() {
        let dir = tempfile::tempdir().unwrap();
        setup_configs(dir.path());
        let ctx = TestContext(dir.path().to_path_buf());

        let mut ports = BTreeMap::new();
        ports.insert(
            "eth1".to_string(),
            PortAuthMode::Dot1xClient {
                guest: ProfileId {
                    fullname: "Nope".into(),
                    interface: "nope".into(),
                    vlan_tag: 4000,
                },
            },
        );
        let req = Dot1xConfig {
            enabled: true,
            role: Dot1xRole::Core,
            upstream: None,
            ports,
        };
        let res = set(ctx, DeserializeStdin(req)).await;
        assert!(res.is_err(), "unknown guest profile must be rejected");
    }

    #[test]
    fn parse_role_defaults_to_core() {
        assert_eq!(parse_role("satellite"), Dot1xRole::Satellite);
        assert_eq!(parse_role("core"), Dot1xRole::Core);
        assert_eq!(parse_role("garbage"), Dot1xRole::Core);
    }
}

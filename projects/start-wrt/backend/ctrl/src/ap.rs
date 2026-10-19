//! What an access point writes to its own configs (design D25).
//!
//! An access point bridges its uplink, its Wi-Fi passwords and its access ports
//! onto the Core's profile VLANs and routes nothing. The base layout is written
//! at adoption; the sync payload adds profiles, passwords and port assignments.

use std::collections::{BTreeMap, BTreeSet};

use uciedit::openwrt::{
    DeviceType, FirewallTarget, InterfaceProto, NetworkBridgeVlan, NetworkDevice, NetworkInterface,
    WifiDevice, WifiDynamicVlan, WifiInterface, WifiMode, WifiStation, WifiVlan,
};
use uciedit::{dump_all, parse_all, Arena, Configs};

use crate::backhaul::{
    self, ensure_backhaul_interface, ensure_udp_rule, ensure_zone, primary, remove_zone, tagged,
    BACKHAUL_INTERFACE, BACKHAUL_VLAN, BACKHAUL_ZONE,
};
use crate::ethernet::{
    find_lan_bridge, DEFAULT_LAN_BRIDGE, DEFAULT_WAN6_INTERFACE, DEFAULT_WAN_INTERFACE,
};
use crate::prelude::*;
use crate::satellite::{self, RouterRole, SyncSnapshot};

pub const ADMIN_VLAN: u16 = 1;
const ADMIN_INTERFACE: &str = "lan";

fn bridge_name(cfgs: &Configs) -> Result<String, Error> {
    Ok(find_lan_bridge(cfgs)?
        .map(|d| d.name)
        .unwrap_or_else(|| DEFAULT_LAN_BRIDGE.to_string()))
}

/// The device the stock `wan` interface sits on: the board's designated uplink.
pub fn uplink_from_wan(cfgs: &Configs) -> Option<String> {
    cfgs["network"]
        .sections
        .iter()
        .find(|s| s.name().as_deref() == Some(DEFAULT_WAN_INTERFACE))
        .and_then(|s| s.get::<NetworkInterface>().ok())
        .map(|i| i.device)
}

fn set_bridge_vlan(cfgs: &mut Configs, vlan: NetworkBridgeVlan) -> Result<(), Error> {
    for section in &mut cfgs["network"].sections {
        let Ok(existing) = section.get::<NetworkBridgeVlan>() else {
            continue;
        };
        if existing.device == vlan.device && existing.vlan == vlan.vlan {
            section.set(&vlan)?;
            return Ok(());
        }
    }
    cfgs["network"].append(&vlan, None)?;
    Ok(())
}

/// A bridge member with no address: hostapd and access ports join it, the
/// Core serves it.
fn set_memberless_interface(cfgs: &mut Configs, name: &str, device: String) -> Result<(), Error> {
    for section in &mut cfgs["network"].sections {
        if section.name().as_deref() != Some(name) {
            continue;
        }
        if section.ty() != "interface" {
            continue;
        }
        section.set(&NetworkInterface {
            device,
            proto: InterfaceProto::NONE,
            ..Default::default()
        })?;
        return Ok(());
    }
    cfgs["network"].append(
        &NetworkInterface {
            device,
            proto: InterfaceProto::NONE,
            ..Default::default()
        },
        Some(name),
    )?;
    Ok(())
}

/// The uplink in the bridge, the backhaul VLAN untagged on it, the Admin VLAN
/// tagged, no WAN, no addresses, no DHCP, no LAN or WAN zones. The backhaul
/// admits adoption and the tunnel; management reaches the access point
/// through the tunnel alone.
pub fn write_base_layout(cfgs: &mut Configs, uplink: &str) -> Result<(), Error> {
    let bridge = bridge_name(cfgs)?;
    let mut found = false;
    for section in &mut cfgs["network"].sections {
        let Ok(mut dev) = section.get::<NetworkDevice>() else {
            continue;
        };
        if dev.ty != Some(DeviceType::BRIDGE) || dev.name != bridge {
            continue;
        }
        found = true;
        if !dev.ports.iter().any(|p| p == uplink) {
            dev.ports.push(uplink.to_string());
            section.set(&dev)?;
        }
    }
    if !found {
        cfgs["network"].append(
            &NetworkDevice {
                name: bridge.clone(),
                ty: Some(DeviceType::BRIDGE),
                ports: vec![uplink.to_string()],
                macaddr: None,
            },
            None,
        )?;
    }
    cfgs["network"].sections.retain(|s| {
        !(s.ty() == "interface"
            && matches!(
                s.name().as_deref(),
                Some(DEFAULT_WAN_INTERFACE) | Some(DEFAULT_WAN6_INTERFACE)
            ))
    });
    set_memberless_interface(cfgs, ADMIN_INTERFACE, format!("{bridge}.{ADMIN_VLAN}"))?;
    ensure_backhaul_interface(cfgs, &bridge)?;
    set_bridge_vlan(
        cfgs,
        NetworkBridgeVlan {
            device: bridge.clone(),
            vlan: BACKHAUL_VLAN,
            ports: vec![primary(uplink)],
        },
    )?;
    set_bridge_vlan(
        cfgs,
        NetworkBridgeVlan {
            device: bridge.clone(),
            vlan: ADMIN_VLAN,
            ports: vec![tagged(uplink)],
        },
    )?;
    cfgs["dhcp"].sections.retain(|s| s.ty() != "dhcp");
    remove_zone(cfgs, "lan");
    remove_zone(cfgs, "wan");
    ensure_zone(
        cfgs,
        BACKHAUL_ZONE,
        FirewallTarget::REJECT,
        BACKHAUL_INTERFACE,
    )?;
    backhaul::ensure_icmpv6_rule(cfgs, BACKHAUL_ZONE)?;
    ensure_udp_rule(
        cfgs,
        backhaul::CANDIDATE_RULE_SECTION,
        backhaul::CANDIDATE_RULE,
        BACKHAUL_ZONE,
        crate::adoption::PORT,
    )?;
    Ok(())
}

/// Every profile VLAN, every access port, every Wi-Fi password, from the payload.
pub fn write_snapshot(
    cfgs: &mut Configs,
    snapshot: &SyncSnapshot,
    uplink: &str,
) -> Result<(), Error> {
    write_base_layout(cfgs, uplink)?;
    let bridge = bridge_name(cfgs)?;
    let tags: BTreeSet<u16> = snapshot.profiles.iter().map(|p| p.vlan_tag).collect();
    for (port, tag) in &snapshot.ports {
        if port == uplink {
            return Err(Error::new(
                eyre!("the uplink port cannot be an access port: {port}"),
                ErrorKind::InvalidRequest,
            ));
        }
        if let Some(tag) = tag {
            if !tags.contains(tag) {
                return Err(Error::new(
                    eyre!("port {port} is on VLAN {tag}, which no profile carries"),
                    ErrorKind::InvalidRequest,
                ));
            }
        }
    }
    for section in &mut cfgs["network"].sections {
        let Ok(mut dev) = section.get::<NetworkDevice>() else {
            continue;
        };
        if dev.ty != Some(DeviceType::BRIDGE) || dev.name != bridge {
            continue;
        }
        let wireless: Vec<String> = dev
            .ports
            .iter()
            .filter(|p| p.starts_with("wlan") || p.starts_with("phy"))
            .cloned()
            .collect();
        dev.ports = std::iter::once(uplink.to_string())
            .chain(snapshot.ports.keys().cloned())
            .chain(wireless)
            .collect();
        section.set(&dev)?;
    }
    cfgs["network"].sections.retain(|s| {
        !s.get::<NetworkBridgeVlan>()
            .is_ok_and(|v| v.device == bridge && v.vlan != BACKHAUL_VLAN && !tags.contains(&v.vlan))
    });
    for profile in &snapshot.profiles {
        let mut ports = vec![tagged(uplink)];
        for (port, tag) in &snapshot.ports {
            if *tag == Some(profile.vlan_tag) {
                ports.push(primary(port));
            }
        }
        set_bridge_vlan(
            cfgs,
            NetworkBridgeVlan {
                device: bridge.clone(),
                vlan: profile.vlan_tag,
                ports,
            },
        )?;
        set_memberless_interface(
            cfgs,
            &profile.interface,
            format!("{bridge}.{}", profile.vlan_tag),
        )?;
    }
    let kept: BTreeSet<&str> = snapshot
        .profiles
        .iter()
        .map(|p| p.interface.as_str())
        .chain([
            BACKHAUL_INTERFACE,
            crate::mgmt_tunnel::AP_INTERFACE,
            "loopback",
        ])
        .collect();
    cfgs["network"].sections.retain(|s| {
        if s.ty() != "interface" {
            return true;
        }
        let Some(name) = s.name() else { return true };
        kept.contains(&*name)
    });
    write_wireless(cfgs, snapshot)?;
    Ok(())
}

fn write_wireless(cfgs: &mut Configs, snapshot: &SyncSnapshot) -> Result<(), Error> {
    let admin_key = snapshot
        .passwords
        .iter()
        .find(|p| p.vlan_tag == ADMIN_VLAN)
        .map(|p| p.key.clone());
    let bands: BTreeMap<String, String> = cfgs["wireless"]
        .sections
        .iter()
        .filter_map(|s| Some((s.name()?.to_string(), s.get::<WifiDevice>().ok()?.band)))
        .collect();
    let mut ap_ifaces: Vec<(String, String)> = Vec::new();
    for section in &mut cfgs["wireless"].sections {
        let Some(mut iface) = section.get_typed::<WifiInterface>()? else {
            continue;
        };
        if iface.mode != WifiMode::AP {
            continue;
        }
        let name = section
            .name()
            .ok_or_else(|| {
                Error::new(
                    eyre!("unnamed wireless interface"),
                    ErrorKind::UnnamedWirelessInterface,
                )
            })?
            .to_string();
        ap_ifaces.push((name, iface.device.clone()));
        let five_ghz = bands.get(&iface.device).is_some_and(|b| b == "5g");
        iface.ssid = if snapshot.broadcast_separately && five_ghz {
            format!("{}-5G", snapshot.ssid)
        } else {
            snapshot.ssid.clone()
        };
        iface.encryption = "psk2".to_string();
        iface.key = admin_key.clone();
        iface.dynamic_vlan = if admin_key.is_some() {
            WifiDynamicVlan::ALLOWED
        } else {
            WifiDynamicVlan::REQUIRED
        };
        iface.hidden = false;
        iface.maxassoc = None;
        section.set(&iface)?;
    }
    for section in &mut cfgs["wireless"].sections {
        let Some(mut device) = section.get_typed::<WifiDevice>()? else {
            continue;
        };
        let Some(name) = section.name().map(|n| n.to_string()) else {
            continue;
        };
        let Some((radio, _)) = ap_ifaces.iter().find(|(_, dev)| *dev == name) else {
            continue;
        };
        device.disabled = false;
        device.country = snapshot.country.clone();
        if let Some(plan) = snapshot.radios.get(radio) {
            device.band = plan.band.clone();
            device.channel = uciedit::openwrt::WifiChannel::Int(plan.channel);
            device.htmode = Some(plan.htmode.clone());
        }
        section.set(&device)?;
    }
    cfgs["wireless"]
        .sections
        .retain(|s| s.ty() != "wifi-station" && s.ty() != "wifi-vlan");
    for password in &snapshot.passwords {
        let vid = (password.vlan_tag != ADMIN_VLAN).then_some(password.vlan_tag);
        for (iface, _) in &ap_ifaces {
            cfgs["wireless"].append(
                &WifiStation {
                    key: password.key.clone(),
                    vid,
                    iface: Some(iface.clone()),
                    label: Some(password.label.clone()),
                },
                None,
            )?;
        }
    }
    for profile in snapshot
        .profiles
        .iter()
        .filter(|p| p.vlan_tag != ADMIN_VLAN)
    {
        for (iface, _) in &ap_ifaces {
            cfgs["wireless"].append(
                &WifiVlan {
                    name: profile.interface.clone(),
                    network: profile.interface.clone(),
                    vid: profile.vlan_tag,
                    iface: Some(iface.clone()),
                },
                None,
            )?;
        }
    }
    Ok(())
}

/// Whether a payload may be applied over what was applied last: never an
/// older one, and the same one has nothing to do.
pub fn accept_generation(applied: u64, incoming: u64) -> Result<bool, Error> {
    use std::cmp::Ordering::*;
    match incoming.cmp(&applied) {
        Less => Err(Error::new(
            eyre!("generation {incoming} is older than the applied {applied}"),
            ErrorKind::InvalidRequest,
        )),
        Equal => Ok(false),
        Greater => Ok(true),
    }
}

/// Applies a payload the Core pushed over the tunnel (D5).
#[instrument(skip_all)]
pub async fn apply_snapshot(snapshot: &SyncSnapshot) -> Result<(), Error> {
    let role = satellite::load_role();
    if role.role != RouterRole::Satellite || role.core_key.is_none() {
        return Err(Error::new(
            eyre!("this router is not an adopted access point"),
            ErrorKind::Authorization,
        ));
    }
    let Some(uplink) = role.uplink else {
        return Err(Error::new(
            eyre!("this access point has no uplink on record"),
            ErrorKind::Uninitialized,
        ));
    };
    if !accept_generation(role.generation, snapshot.generation)? {
        return Ok(());
    }
    let mut retries = 4;
    loop {
        let arena = Arena::new();
        let mut cfgs = parse_all(
            "/etc/config",
            &arena,
            &["network", "firewall", "dhcp", "wireless"],
        )
        .await?;
        write_snapshot(&mut cfgs, snapshot, &uplink)?;
        match dump_all("/etc/config", cfgs).await {
            Err(uciedit::Error::Conflict { .. }) if retries > 0 => {
                retries -= 1;
                continue;
            }
            Err(e) => return Err(e.into()),
            Ok(()) => break,
        }
    }
    satellite::persist_generation(snapshot.generation).await?;
    reload_all().await;
    crate::activity::log(
        "satellite",
        "apply",
        true,
        &format!("Applied configuration generation {}", snapshot.generation),
        None,
    );
    Ok(())
}

/// Network before Wi-Fi: hostapd attaches its interfaces to the bridge after
/// netifd has rebuilt the VLAN table.
pub async fn reload_all() {
    let _ =
        crate::run_quiet_async(tokio::process::Command::new("/etc/init.d/network").arg("reload"))
            .await;
    let _ =
        crate::run_quiet_async(tokio::process::Command::new("/etc/init.d/firewall").arg("reload"))
            .await;
    let _ =
        crate::run_quiet_async(tokio::process::Command::new("/etc/init.d/dnsmasq").arg("reload"))
            .await;
    let _ = crate::run_quiet_async(&mut tokio::process::Command::new("wifi")).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::satellite::{PasswordSpec, ProfileSpec};

    const NETWORK: &str = "\
config device\n\toption name 'br-lan'\n\toption type 'bridge'\n\tlist ports 'eth0'\n\n\
config interface 'lan'\n\toption device 'br-lan'\n\toption proto 'static'\n\toption ipaddr '192.168.0.1'\n\toption netmask '255.255.255.0'\n\n\
config interface 'wan'\n\toption device 'eth1'\n\toption proto 'dhcp'\n\n\
config interface 'wan6'\n\toption device 'eth1'\n\toption proto 'dhcpv6'\n";
    const WIRELESS: &str = "\
config wifi-device 'radio0'\n\toption type 'mac80211'\n\toption band '2g'\n\toption channel '1'\n\toption htmode 'HE20'\n\toption disabled '1'\n\n\
config wifi-iface 'default_radio0'\n\toption device 'radio0'\n\toption network 'lan'\n\toption mode 'ap'\n\toption ssid 'StartWRT'\n\toption encryption 'none'\n";
    const FIREWALL: &str = "\
config zone\n\toption name 'lan'\n\toption input 'ACCEPT'\n\toption output 'ACCEPT'\n\toption forward 'ACCEPT'\n\tlist network 'lan'\n\n\
config zone\n\toption name 'wan'\n\toption input 'REJECT'\n\toption output 'ACCEPT'\n\toption forward 'REJECT'\n\tlist network 'wan'\n\tlist network 'wan6'\n\n\
config forwarding\n\toption src 'lan'\n\toption dest 'wan'\n";
    const DHCP: &str = "\
config dhcp 'lan'\n\toption interface 'lan'\n\toption start '100'\n\toption limit '150'\n\toption leasetime '12h'\n";

    async fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (f, c) in [
            ("network", NETWORK),
            ("wireless", WIRELESS),
            ("firewall", FIREWALL),
            ("dhcp", DHCP),
        ] {
            tokio::fs::write(dir.path().join(f), c).await.unwrap();
        }
        dir
    }

    fn snapshot() -> SyncSnapshot {
        SyncSnapshot {
            generation: 3,
            ssid: "Home".into(),
            broadcast_separately: false,
            country: Some("US".into()),
            radios: BTreeMap::new(),
            profiles: vec![
                ProfileSpec {
                    fullname: "Admin".into(),
                    interface: "lan".into(),
                    vlan_tag: 1,
                },
                ProfileSpec {
                    fullname: "guest".into(),
                    interface: "yyiph".into(),
                    vlan_tag: 101,
                },
            ],
            passwords: vec![
                PasswordSpec {
                    label: "Default".into(),
                    key: "adminpassword".into(),
                    vlan_tag: 1,
                },
                PasswordSpec {
                    label: "Guest".into(),
                    key: "guestpassword".into(),
                    vlan_tag: 101,
                },
            ],
            ports: BTreeMap::from([("eth0".to_string(), Some(101))]),
            update_source: None,
        }
    }

    fn dump(cfgs: &Configs, name: &str) -> String {
        cfgs[name]
            .sections
            .iter()
            .flat_map(|s| s.lines.iter().map(|l| l.to_string()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn base_layout_bridges_the_uplink_and_drops_wan_and_dhcp() {
        let dir = fixture().await;
        let arena = Arena::new();
        let mut cfgs = parse_all(
            dir.path(),
            &arena,
            &["network", "firewall", "dhcp", "wireless"],
        )
        .await
        .unwrap();
        write_base_layout(&mut cfgs, "eth1").unwrap();
        write_base_layout(&mut cfgs, "eth1").unwrap();
        let net = dump(&cfgs, "network");
        assert!(net.contains("list ports 'eth1'"), "{net}");
        assert!(
            !net.contains("interface wan") && !net.contains("interface 'wan'"),
            "{net}"
        );
        assert!(!net.contains("ipaddr"), "{net}");
        assert_eq!(net.matches("option vlan '4094'").count(), 1, "{net}");
        assert!(net.contains("list ports 'eth1:u*'"), "{net}");
        assert!(net.contains("list ports 'eth1:t'"), "{net}");
        assert!(dump(&cfgs, "dhcp").is_empty());
        let fw = dump(&cfgs, "firewall");
        assert!(
            !fw.contains("name 'lan'") && !fw.contains("name 'wan'"),
            "{fw}"
        );
        assert!(fw.contains("name 'backhaul'"), "{fw}");
        assert!(fw.contains("dest_port '8719'"), "{fw}");
        assert!(!fw.contains("dest_port '22'"), "{fw}");
    }

    #[tokio::test]
    async fn snapshot_writes_profile_vlans_ports_and_wifi() {
        let dir = fixture().await;
        let arena = Arena::new();
        let mut cfgs = parse_all(
            dir.path(),
            &arena,
            &["network", "firewall", "dhcp", "wireless"],
        )
        .await
        .unwrap();
        write_snapshot(&mut cfgs, &snapshot(), "eth1").unwrap();
        let net = dump(&cfgs, "network");
        assert!(net.contains("option vlan '101'"), "{net}");
        assert!(net.contains("list ports 'eth0:u*'"), "{net}");
        assert!(
            net.contains("interface yyiph") || net.contains("interface 'yyiph'"),
            "{net}"
        );
        assert!(net.contains("option device 'br-lan.101'"), "{net}");
        assert!(
            net.contains("interface backhaul") || net.contains("interface 'backhaul'"),
            "{net}"
        );
        let wl = dump(&cfgs, "wireless");
        assert!(wl.contains("option ssid 'Home'"), "{wl}");
        assert!(wl.contains("option key 'adminpassword'"), "{wl}");
        assert!(wl.contains("option dynamic_vlan '1'"), "{wl}");
        assert!(wl.contains("option disabled '0'"), "{wl}");
        assert!(wl.contains("option country 'US'"), "{wl}");
        assert!(wl.contains("option vid '101'"), "{wl}");
        assert!(wl.contains("option key 'guestpassword'"), "{wl}");
        assert_eq!(wl.matches("config wifi-vlan").count(), 1, "{wl}");
        assert_eq!(wl.matches("config wifi-station").count(), 2, "{wl}");
    }

    #[test]
    fn generations_only_move_forward() {
        assert!(accept_generation(3, 4).unwrap());
        assert!(!accept_generation(3, 3).unwrap());
        assert!(accept_generation(3, 2).is_err());
    }

    #[tokio::test]
    async fn snapshot_refuses_an_access_port_on_an_unknown_vlan() {
        let dir = fixture().await;
        let arena = Arena::new();
        let mut cfgs = parse_all(
            dir.path(),
            &arena,
            &["network", "firewall", "dhcp", "wireless"],
        )
        .await
        .unwrap();
        let mut s = snapshot();
        s.ports.insert("eth0".into(), Some(7));
        assert!(write_snapshot(&mut cfgs, &s, "eth1").is_err());
        let mut s = snapshot();
        s.ports.insert("eth1".into(), None);
        assert!(write_snapshot(&mut cfgs, &s, "eth1").is_err());
    }
}

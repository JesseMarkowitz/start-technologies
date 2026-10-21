//! The trunk between a main router and its access points (design D25).
//!
//! A trunk port on the main router carries every profile VLAN tagged and the
//! backhaul VLAN untagged. The backhaul VLAN is not a profile: it carries IPv6
//! link-local only, for adoption and each access point's management tunnel. An
//! access point's uplink port is the mirror image, so both roles share the
//! constants and the backhaul interface and zone here.

use std::collections::BTreeSet;

use uciedit::openwrt::{
    FirewallForwarding, FirewallRule, FirewallTarget, FirewallZone, InterfaceProto,
    NetworkBridgeVlan, NetworkInterface, NetworkVlanPort, NetworkVlanPortTagging,
};
use uciedit::Configs;

use crate::prelude::*;

/// The last valid VLAN id, outside the profile allocator's reach in practice.
pub const BACKHAUL_VLAN: u16 = 4094;
pub const BACKHAUL_INTERFACE: &str = "backhaul";
pub const BACKHAUL_ZONE: &str = "backhaul";
/// The candidate's rule, kept by name when its zone is replaced at adoption.
pub const CANDIDATE_RULE: &str = "Allow-Adoption";
pub const CANDIDATE_RULE_SECTION: &str = "allow_adoption";
const ADOPTION_RULE: &str = "Allow-Adoption-Backhaul";
const ADOPTION_RULE_SECTION: &str = "allow_adoption_backhaul";

pub fn backhaul_device(bridge: &str) -> String {
    format!("{bridge}.{BACKHAUL_VLAN}")
}

/// Ports that are PRIMARY on the backhaul VLAN of any bridge: the main router's
/// trunk ports, or an access point's uplink.
pub fn trunk_ports(cfgs: &Configs) -> BTreeSet<String> {
    cfgs["network"]
        .sections
        .iter()
        .filter_map(|s| s.get::<NetworkBridgeVlan>().ok())
        .filter(|v| v.vlan == BACKHAUL_VLAN)
        .flat_map(|v| v.ports)
        .filter(|p| p.tagging == Some(NetworkVlanPortTagging::PRIMARY))
        .map(|p| p.port)
        .collect()
}

pub fn tagged(port: &str) -> NetworkVlanPort {
    NetworkVlanPort {
        port: port.to_string(),
        tagging: Some(NetworkVlanPortTagging::TAGGED),
    }
}

pub fn primary(port: &str) -> NetworkVlanPort {
    NetworkVlanPort {
        port: port.to_string(),
        tagging: Some(NetworkVlanPortTagging::PRIMARY),
    }
}

pub fn backhaul_bridge_vlan(bridge: &str, trunks: &BTreeSet<String>) -> NetworkBridgeVlan {
    NetworkBridgeVlan {
        device: bridge.to_string(),
        vlan: BACKHAUL_VLAN,
        ports: trunks.iter().map(|p| primary(p)).collect(),
    }
}

/// The backhaul interface: no address, no DHCP, kernel link-local only.
pub fn ensure_backhaul_interface(cfgs: &mut Configs, bridge: &str) -> Result<(), Error> {
    let device = backhaul_device(bridge);
    for section in &mut cfgs["network"].sections {
        if section.name().as_deref() != Some(BACKHAUL_INTERFACE) {
            continue;
        }
        let Ok(mut iface) = section.get::<NetworkInterface>() else {
            continue;
        };
        if iface.device != device || iface.proto != InterfaceProto::NONE {
            iface.device = device;
            iface.proto = InterfaceProto::NONE;
            section.set(&iface)?;
        }
        return Ok(());
    }
    cfgs["network"].append(
        &NetworkInterface {
            device,
            proto: InterfaceProto::NONE,
            ..Default::default()
        },
        Some(BACKHAUL_INTERFACE),
    )?;
    Ok(())
}

pub fn remove_backhaul_interface(cfgs: &mut Configs) {
    cfgs["network"]
        .sections
        .retain(|s| !(s.ty() == "interface" && s.name().as_deref() == Some(BACKHAUL_INTERFACE)));
}

/// A zone that rejects everything but what rules admit, holding `member`.
pub fn ensure_zone(
    cfgs: &mut Configs,
    name: &str,
    input: FirewallTarget,
    member: &str,
) -> Result<(), Error> {
    for section in &mut cfgs["firewall"].sections {
        let Ok(mut zone) = section.get::<FirewallZone>() else {
            continue;
        };
        if zone.name != name {
            continue;
        }
        let mut changed = false;
        if !zone.network.iter().any(|n| n == member) {
            zone.network.push(member.to_string());
            changed = true;
        }
        if zone.input != input {
            zone.input = input;
            changed = true;
        }
        if changed {
            section.set(&zone)?;
        }
        return Ok(());
    }
    cfgs["firewall"].append(
        &FirewallZone {
            name: name.to_string(),
            input,
            output: FirewallTarget::ACCEPT,
            forward: FirewallTarget::REJECT,
            network: vec![member.to_string()],
            masq: None,
            masq6: None,
            mtu_fix: None,
        },
        Some(name),
    )?;
    Ok(())
}

pub fn remove_zone(cfgs: &mut Configs, name: &str) {
    cfgs["firewall"].sections.retain(|s| {
        if let Ok(zone) = s.get::<FirewallZone>() {
            return zone.name != name;
        }
        if let Ok(rule) = s.get::<FirewallRule>() {
            return rule.src != name && rule.dest.as_deref() != Some(name);
        }
        if let Ok(fwd) = s.get::<FirewallForwarding>() {
            return fwd.src != name && fwd.dest != name;
        }
        true
    });
}

/// Admits one UDP port from `zone`, replacing a rule of the same name.
pub fn ensure_udp_rule(
    cfgs: &mut Configs,
    section_name: &str,
    rule_name: &str,
    zone: &str,
    port: u16,
) -> Result<(), Error> {
    let port = port.to_string();
    for section in &mut cfgs["firewall"].sections {
        let Ok(mut rule) = section.get::<FirewallRule>() else {
            continue;
        };
        if rule.name != rule_name {
            continue;
        }
        if rule.src != zone || rule.dest_port.as_deref() != Some(&port) {
            rule.src = zone.to_string();
            rule.dest_port = Some(port.clone());
            section.set(&rule)?;
        }
        return Ok(());
    }
    cfgs["firewall"].append(
        &FirewallRule {
            name: rule_name.to_string(),
            src: zone.to_string(),
            dest_port: Some(port),
            proto: vec!["udp".to_string()],
            target: FirewallTarget::ACCEPT,
            ..Default::default()
        },
        Some(section_name),
    )?;
    Ok(())
}

/// Neighbour discovery and echo over the backhaul; a link-local peer is
/// unreachable without it.
pub fn ensure_icmpv6_rule(cfgs: &mut Configs, zone: &str) -> Result<(), Error> {
    let section = format!("allow_icmpv6_{zone}");
    let name = format!("Allow-ICMPv6-{zone}");
    if cfgs["firewall"]
        .sections
        .iter()
        .any(|s| s.get::<FirewallRule>().is_ok_and(|r| r.name == name))
    {
        return Ok(());
    }
    cfgs["firewall"].append(
        &FirewallRule {
            name,
            src: zone.to_string(),
            proto: vec!["icmp".to_string()],
            icmp_type: [
                "echo-request",
                "echo-reply",
                "destination-unreachable",
                "packet-too-big",
                "time-exceeded",
                "router-solicitation",
                "neighbour-solicitation",
                "router-advertisement",
                "neighbour-advertisement",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            family: Some("ipv6".to_string()),
            target: FirewallTarget::ACCEPT,
            ..Default::default()
        },
        Some(section.as_str()),
    )?;
    Ok(())
}

/// The main router's backhaul: present while any trunk port exists, gone
/// otherwise. Management tunnels keep their own accept rules on the zone.
pub fn sync_main_backhaul(cfgs: &mut Configs, bridge: &str, has_trunk: bool) -> Result<(), Error> {
    if has_trunk {
        ensure_backhaul_interface(cfgs, bridge)?;
        ensure_zone(
            cfgs,
            BACKHAUL_ZONE,
            FirewallTarget::REJECT,
            BACKHAUL_INTERFACE,
        )?;
        ensure_icmpv6_rule(cfgs, BACKHAUL_ZONE)?;
        ensure_udp_rule(
            cfgs,
            ADOPTION_RULE_SECTION,
            ADOPTION_RULE,
            BACKHAUL_ZONE,
            crate::adoption::PORT,
        )?;
    } else {
        remove_zone(cfgs, BACKHAUL_ZONE);
        remove_backhaul_interface(cfgs);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use uciedit::{parse_all, Arena};

    use super::*;

    const NETWORK: &str = "\
config device\n\toption name 'br-lan'\n\toption type 'bridge'\n\tlist ports 'eth0'\n\tlist ports 'eth2'\n\n\
config interface 'lan'\n\toption device 'br-lan.1'\n\toption proto 'static'\n\toption ipaddr '192.168.0.1'\n\toption netmask '255.255.255.0'\n\n\
config bridge-vlan\n\toption device 'br-lan'\n\toption vlan '1'\n\tlist ports 'eth0:u*'\n\tlist ports 'eth2:t'\n\n\
config bridge-vlan\n\toption device 'br-lan'\n\toption vlan '4094'\n\tlist ports 'eth2:u*'\n";

    async fn fixture(dir: &std::path::Path) {
        tokio::fs::write(dir.join("network"), NETWORK)
            .await
            .unwrap();
        tokio::fs::write(dir.join("firewall"), "").await.unwrap();
    }

    #[tokio::test]
    async fn trunk_is_the_primary_member_of_the_backhaul_vlan() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path()).await;
        let arena = Arena::new();
        let cfgs = parse_all(dir.path(), &arena, &["network"]).await.unwrap();
        let trunks = trunk_ports(&cfgs);
        assert_eq!(trunks.into_iter().collect::<Vec<_>>(), vec!["eth2"]);
    }

    #[tokio::test]
    async fn main_backhaul_comes_and_goes_with_the_trunk() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path()).await;
        let arena = Arena::new();
        let mut cfgs = parse_all(dir.path(), &arena, &["network", "firewall"])
            .await
            .unwrap();
        sync_main_backhaul(&mut cfgs, "br-lan", true).unwrap();
        sync_main_backhaul(&mut cfgs, "br-lan", true).unwrap();
        let ifaces: Vec<_> = cfgs["network"]
            .sections
            .iter()
            .filter(|s| s.name().as_deref() == Some(BACKHAUL_INTERFACE))
            .collect();
        assert_eq!(ifaces.len(), 1, "idempotent interface");
        let iface = ifaces[0].get::<NetworkInterface>().unwrap();
        assert_eq!(iface.device, "br-lan.4094");
        assert_eq!(iface.proto, InterfaceProto::NONE);
        let zones = cfgs["firewall"]
            .sections
            .iter()
            .filter(|s| {
                s.get::<FirewallZone>()
                    .is_ok_and(|z| z.name == BACKHAUL_ZONE)
            })
            .count();
        assert_eq!(zones, 1);
        let rules = cfgs["firewall"]
            .sections
            .iter()
            .filter(|s| {
                s.get::<FirewallRule>()
                    .is_ok_and(|r| r.name == ADOPTION_RULE)
            })
            .count();
        assert_eq!(rules, 1);
        assert!(cfgs["firewall"].sections.iter().any(|s| {
            s.get::<FirewallRule>()
                .is_ok_and(|r| r.name == "Allow-ICMPv6-backhaul" && r.icmp_type.len() == 9)
        }));

        sync_main_backhaul(&mut cfgs, "br-lan", false).unwrap();
        assert!(cfgs["network"]
            .sections
            .iter()
            .all(|s| s.name().as_deref() != Some(BACKHAUL_INTERFACE)));
        assert!(cfgs["firewall"].sections.iter().all(|s| {
            !s.get::<FirewallZone>()
                .is_ok_and(|z| z.name == BACKHAUL_ZONE)
                && !s
                    .get::<FirewallRule>()
                    .is_ok_and(|r| r.src == BACKHAUL_ZONE)
        }));
    }
}

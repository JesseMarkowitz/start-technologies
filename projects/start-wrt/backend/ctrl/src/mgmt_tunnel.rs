//! The management tunnel between a Core and one access point (design D25).
//!
//! One WireGuard interface per access point on the Core, one on the access
//! point, over IPv6 link-local on the backhaul VLAN. It carries sync, status,
//! liveness and logs; profile traffic never enters it.

use std::net::IpAddr;

use uciedit::openwrt::FirewallTarget;
use uciedit::{Arena, Configs, Line, LineComment, Section, Token};

use crate::backhaul::{ensure_udp_rule, ensure_zone, BACKHAUL_ZONE};
use crate::prelude::*;
use crate::vpn_server::WgInterface;

/// The Core's zone holding every management tunnel.
pub const CORE_ZONE: &str = "satellites";
/// The access point's tunnel interface and zone.
pub const AP_INTERFACE: &str = "core";
pub const AP_ZONE: &str = "core";
pub const KEEPALIVE: &str = "25";

/// Linux `IFNAMSIZ` is 15 characters, so a registry name is at most 11.
pub fn core_interface_name(satellite: &str) -> String {
    format!("sat_{satellite}")
}

fn host_cidr(addr: IpAddr) -> String {
    match addr {
        IpAddr::V4(a) => format!("{a}/32"),
        IpAddr::V6(a) => format!("{a}/128"),
    }
}

fn wg_rule_names(interface: &str) -> (String, String) {
    (
        format!("allow_wireguard_{interface}"),
        format!("Allow-WireGuard-{interface}"),
    )
}

fn set_interface(
    cfgs: &mut Configs,
    name: &str,
    private_key: &str,
    listen_port: Option<u16>,
    address: IpAddr,
    nohostroute: Option<bool>,
) -> Result<(), Error> {
    let addresses = vec![host_cidr(address)];
    for section in &mut cfgs["network"].sections {
        if section.name().as_deref() != Some(name) {
            continue;
        }
        let Ok(mut wg) = section.get::<WgInterface>() else {
            continue;
        };
        if !wg.is_wireguard() {
            continue;
        }
        wg.private_key = private_key.to_string();
        wg.listen_port = listen_port;
        wg.addresses = addresses;
        wg.nohostroute = nohostroute;
        section.set(&wg)?;
        return Ok(());
    }
    cfgs["network"].append(
        &WgInterface {
            proto: "wireguard".to_string(),
            private_key: private_key.to_string(),
            listen_port,
            addresses,
            disabled: None,
            mtu: None,
            nohostroute,
        },
        Some(name),
    )?;
    Ok(())
}

struct Peer<'p> {
    public_key: &'p str,
    preshared_key: &'p str,
    description: String,
    allowed_ip: String,
    endpoint: Option<(&'p str, u16)>,
}

/// Exactly one peer per tunnel; re-provisioning replaces it.
fn set_peer<'a>(
    cfgs: &mut Configs<'a>,
    arena: &'a Arena,
    interface: &str,
    peer: &Peer,
) -> Result<(), Error> {
    let peer_type: &str = arena.alloc(format!("wireguard_{interface}"));
    cfgs["network"].sections.retain(|s| s.ty() != peer_type);
    let peer_name: &str = arena.alloc(format!("{interface}_peer"));
    let mut lines = vec![Line::Section {
        ty: Token::from_str(peer_type, arena),
        name: Some(Token::from_str(peer_name, arena)),
        comment: LineComment::None,
    }];
    let mut option = |option: &'a str, value: String| {
        let value: &str = arena.alloc(value);
        lines.push(Line::Option {
            option: Token::from_str(option, arena),
            value: Token::from_str(value, arena),
            comment: LineComment::None,
        });
    };
    option("public_key", peer.public_key.to_string());
    option("description", peer.description.clone());
    option("preshared_key", peer.preshared_key.to_string());
    option("persistent_keepalive", KEEPALIVE.to_string());
    option("route_allowed_ips", "1".to_string());
    if let Some((host, port)) = peer.endpoint {
        option("endpoint_host", host.to_string());
        option("endpoint_port", port.to_string());
    }
    let allowed: &str = arena.alloc(peer.allowed_ip.clone());
    lines.push(Line::List {
        list: Token::from_str("allowed_ips", arena),
        item: Token::from_str(allowed, arena),
        comment: LineComment::None,
    });
    cfgs["network"].sections.push(Section { arena, lines });
    Ok(())
}

pub struct CoreEnd<'p> {
    pub satellite: &'p str,
    pub core_addr: IpAddr,
    pub satellite_addr: IpAddr,
    pub satellite_public_key: &'p str,
    pub preshared_key: &'p str,
    pub listen_port: u16,
    pub core_private_key: &'p str,
}

/// The Core's end: listens on the backhaul, roams to wherever the access
/// point's handshake comes from.
pub fn provision_core_end<'a>(
    cfgs: &mut Configs<'a>,
    arena: &'a Arena,
    end: &CoreEnd,
) -> Result<(), Error> {
    let interface = core_interface_name(end.satellite);
    set_interface(
        cfgs,
        &interface,
        end.core_private_key,
        Some(end.listen_port),
        end.core_addr,
        None,
    )?;
    set_peer(
        cfgs,
        arena,
        &interface,
        &Peer {
            public_key: end.satellite_public_key,
            preshared_key: end.preshared_key,
            description: format!("satellite {}", end.satellite),
            allowed_ip: host_cidr(end.satellite_addr),
            endpoint: None,
        },
    )?;
    ensure_zone(cfgs, CORE_ZONE, FirewallTarget::ACCEPT, &interface)?;
    let (section, name) = wg_rule_names(&interface);
    ensure_udp_rule(cfgs, &section, &name, BACKHAUL_ZONE, end.listen_port)?;
    Ok(())
}

/// Removes the Core's end of one access point's tunnel.
pub fn remove_core_end(cfgs: &mut Configs, satellite: &str) -> Result<(), Error> {
    let interface = core_interface_name(satellite);
    let peer_type = format!("wireguard_{interface}");
    cfgs["network"].sections.retain(|s| {
        s.ty() != peer_type
            && !(s.ty() == "interface" && s.name().as_deref() == Some(interface.as_str()))
    });
    let (_, rule_name) = wg_rule_names(&interface);
    cfgs["firewall"].sections.retain(|s| {
        !s.get::<uciedit::openwrt::FirewallRule>()
            .is_ok_and(|r| r.name == rule_name)
    });
    for section in &mut cfgs["firewall"].sections {
        let Ok(mut zone) = section.get::<uciedit::openwrt::FirewallZone>() else {
            continue;
        };
        if zone.name == CORE_ZONE && zone.network.iter().any(|n| *n == interface) {
            zone.network.retain(|n| *n != interface);
            section.set(&zone)?;
        }
    }
    Ok(())
}

pub struct ApEnd<'p> {
    pub satellite_addr: IpAddr,
    pub core_addr: IpAddr,
    pub core_public_key: &'p str,
    /// `<link-local>%<interface>`; netifd brackets a host with a colon itself.
    pub core_endpoint_host: &'p str,
    pub core_endpoint_port: u16,
    pub preshared_key: &'p str,
    pub private_key: &'p str,
}

/// The access point's end dials the Core and never listens.
pub fn provision_ap_end<'a>(
    cfgs: &mut Configs<'a>,
    arena: &'a Arena,
    end: &ApEnd,
) -> Result<(), Error> {
    let nohostroute = end.core_endpoint_host.contains('%').then_some(true);
    set_interface(
        cfgs,
        AP_INTERFACE,
        end.private_key,
        None,
        end.satellite_addr,
        nohostroute,
    )?;
    set_peer(
        cfgs,
        arena,
        AP_INTERFACE,
        &Peer {
            public_key: end.core_public_key,
            preshared_key: end.preshared_key,
            description: "core".to_string(),
            allowed_ip: host_cidr(end.core_addr),
            endpoint: Some((end.core_endpoint_host, end.core_endpoint_port)),
        },
    )?;
    ensure_zone(cfgs, AP_ZONE, FirewallTarget::ACCEPT, AP_INTERFACE)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use uciedit::parse_all;

    use super::*;

    async fn fixture(dir: &std::path::Path) {
        tokio::fs::write(dir.join("network"), "").await.unwrap();
        tokio::fs::write(dir.join("firewall"), "").await.unwrap();
    }

    fn core_end() -> CoreEnd<'static> {
        CoreEnd {
            satellite: "s1",
            core_addr: "10.42.1.1".parse().unwrap(),
            satellite_addr: "10.42.1.2".parse().unwrap(),
            satellite_public_key: "SATKEY=",
            preshared_key: "PSK=",
            listen_port: 51900,
            core_private_key: "COREPRIV=",
        }
    }

    #[tokio::test]
    async fn core_end_is_idempotent_and_removable() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path()).await;
        let arena = Arena::new();
        let mut cfgs = parse_all(dir.path(), &arena, &["network", "firewall"])
            .await
            .unwrap();
        provision_core_end(&mut cfgs, &arena, &core_end()).unwrap();
        provision_core_end(&mut cfgs, &arena, &core_end()).unwrap();
        let ifaces = cfgs["network"]
            .sections
            .iter()
            .filter(|s| s.name().as_deref() == Some("sat_s1"))
            .count();
        let peers = cfgs["network"]
            .sections
            .iter()
            .filter(|s| s.ty() == "wireguard_sat_s1")
            .count();
        assert_eq!((ifaces, peers), (1, 1));
        let wg = cfgs["network"]
            .sections
            .iter()
            .find(|s| s.name().as_deref() == Some("sat_s1"))
            .unwrap()
            .get::<WgInterface>()
            .unwrap();
        assert_eq!(wg.listen_port, Some(51900));
        assert_eq!(wg.addresses, vec!["10.42.1.1/32"]);
        let zone = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<uciedit::openwrt::FirewallZone>().ok())
            .find(|z| z.name == CORE_ZONE)
            .unwrap();
        assert_eq!(zone.network, vec!["sat_s1"]);
        let rule = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<uciedit::openwrt::FirewallRule>().ok())
            .find(|r| r.name == "Allow-WireGuard-sat_s1")
            .unwrap();
        assert_eq!(
            (rule.src.as_str(), rule.dest_port.as_deref()),
            (BACKHAUL_ZONE, Some("51900"))
        );

        remove_core_end(&mut cfgs, "s1").unwrap();
        assert!(cfgs["network"]
            .sections
            .iter()
            .all(|s| s.name().as_deref() != Some("sat_s1") && s.ty() != "wireguard_sat_s1"));
        let zone = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<uciedit::openwrt::FirewallZone>().ok())
            .find(|z| z.name == CORE_ZONE)
            .unwrap();
        assert!(zone.network.is_empty());
    }

    #[tokio::test]
    async fn ap_end_dials_a_scoped_endpoint_without_a_host_route() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path()).await;
        let arena = Arena::new();
        let mut cfgs = parse_all(dir.path(), &arena, &["network", "firewall"])
            .await
            .unwrap();
        let end = ApEnd {
            satellite_addr: "10.42.1.2".parse().unwrap(),
            core_addr: "10.42.1.1".parse().unwrap(),
            core_public_key: "COREKEY=",
            core_endpoint_host: "fe80::1%br-lan.4094",
            core_endpoint_port: 51900,
            preshared_key: "PSK=",
            private_key: "APPRIV=",
        };
        provision_ap_end(&mut cfgs, &arena, &end).unwrap();
        let wg = cfgs["network"]
            .sections
            .iter()
            .find(|s| s.name().as_deref() == Some(AP_INTERFACE))
            .unwrap()
            .get::<WgInterface>()
            .unwrap();
        assert_eq!(wg.nohostroute, Some(true));
        assert_eq!(wg.listen_port, None);
        let peer = cfgs["network"]
            .sections
            .iter()
            .find(|s| s.ty() == "wireguard_core")
            .unwrap();
        let text = peer
            .lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("endpoint_host") && text.contains("fe80::1%br-lan.4094"),
            "{text}"
        );
        assert!(
            text.contains("allowed_ips") && text.contains("10.42.1.1/32"),
            "{text}"
        );
    }
}

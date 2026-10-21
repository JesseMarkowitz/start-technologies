//! The management tunnel between a main router and one access point
//! (design D25).
//!
//! One WireGuard interface per access point on the main router, one on the
//! access point, over IPv6 link-local on the backhaul VLAN. It carries sync,
//! status, liveness, logs, firmware and the access point's time; profile
//! traffic never enters it.
//!
//! Both ends keep a persistent keepalive and both can initiate: the access
//! point listens on a fixed port, and the main router re-pins the endpoint it
//! last learned (`liveness`), so an access point that reboots with a stale
//! clock, whose own handshakes the main router's replay check drops, is reached
//! from the main router's side.

use std::net::IpAddr;

use uciedit::openwrt::{FirewallRule, FirewallTarget, Timeserver};
use uciedit::{Arena, Configs, Line, LineComment, Section, Token};

use crate::backhaul::{ensure_udp_rule, ensure_zone, remove_zone, BACKHAUL_ZONE};
use crate::prelude::*;
use crate::vpn_server::WgInterface;

/// The main router's zone holding every management tunnel.
pub const AP_ZONE: &str = "ap_mgmt";
/// The access point's tunnel interface and zone.
pub const MAIN_INTERFACE: &str = "main";
pub const MAIN_ZONE: &str = "main";
/// Below the main router's listen ports, which start at 51900.
pub const AP_LISTEN_PORT: u16 = 51899;
pub const KEEPALIVE: &str = "25";
/// The tunnel's two addresses share a /24, which makes the access point a
/// local client of the main router's dnsmasq.
const PREFIX_LEN: u8 = 24;

/// Linux `IFNAMSIZ` caps the result at 15 characters.
pub fn ap_interface_name(access_point: &str) -> String {
    format!("ap_{access_point}")
}

fn host_cidr(addr: IpAddr) -> String {
    match addr {
        IpAddr::V4(a) => format!("{a}/32"),
        IpAddr::V6(a) => format!("{a}/128"),
    }
}

fn tunnel_cidr(addr: IpAddr) -> String {
    match addr {
        IpAddr::V4(a) => format!("{a}/{PREFIX_LEN}"),
        IpAddr::V6(a) => format!("{a}/64"),
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
    dns: Vec<String>,
) -> Result<(), Error> {
    let addresses = vec![tunnel_cidr(address)];
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
        wg.dns = dns;
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
            dns,
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

pub struct MainEnd<'p> {
    pub access_point: &'p str,
    pub main_addr: IpAddr,
    pub access_point_addr: IpAddr,
    pub access_point_public_key: &'p str,
    pub preshared_key: &'p str,
    pub listen_port: u16,
    pub main_private_key: &'p str,
}

/// The main router's end: listens on the backhaul, learns the access point's
/// endpoint from its first handshake.
pub fn provision_main_end<'a>(
    cfgs: &mut Configs<'a>,
    arena: &'a Arena,
    end: &MainEnd,
) -> Result<(), Error> {
    let interface = ap_interface_name(end.access_point);
    set_interface(
        cfgs,
        &interface,
        end.main_private_key,
        Some(end.listen_port),
        end.main_addr,
        None,
        Vec::new(),
    )?;
    set_peer(
        cfgs,
        arena,
        &interface,
        &Peer {
            public_key: end.access_point_public_key,
            preshared_key: end.preshared_key,
            description: format!("access point {}", end.access_point),
            allowed_ip: host_cidr(end.access_point_addr),
            endpoint: None,
        },
    )?;
    ensure_zone(cfgs, AP_ZONE, FirewallTarget::ACCEPT, &interface)?;
    let (section, name) = wg_rule_names(&interface);
    ensure_udp_rule(cfgs, &section, &name, BACKHAUL_ZONE, end.listen_port)?;
    Ok(())
}

/// Removes the main router's end of one access point's tunnel.
pub fn remove_main_end(cfgs: &mut Configs, access_point: &str) -> Result<(), Error> {
    let interface = ap_interface_name(access_point);
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
        if zone.name == AP_ZONE && zone.network.iter().any(|n| *n == interface) {
            zone.network.retain(|n| *n != interface);
            section.set(&zone)?;
        }
    }
    Ok(())
}

pub struct ApEnd<'p> {
    pub access_point_addr: IpAddr,
    pub main_addr: IpAddr,
    pub main_public_key: &'p str,
    /// `<link-local>%<interface>`; netifd brackets a host with a colon itself.
    pub main_endpoint_host: &'p str,
    pub main_endpoint_port: u16,
    pub preshared_key: &'p str,
    pub private_key: &'p str,
}

/// The access point's end dials the main router, listens on [`AP_LISTEN_PORT`]
/// for the main router's own initiations, and routes everything that is not
/// on-link through the tunnel.
pub fn provision_ap_end<'a>(
    cfgs: &mut Configs<'a>,
    arena: &'a Arena,
    end: &ApEnd,
) -> Result<(), Error> {
    let nohostroute = end.main_endpoint_host.contains('%').then_some(true);
    set_interface(
        cfgs,
        MAIN_INTERFACE,
        end.private_key,
        Some(AP_LISTEN_PORT),
        end.access_point_addr,
        nohostroute,
        vec![end.main_addr.to_string()],
    )?;
    set_peer(
        cfgs,
        arena,
        MAIN_INTERFACE,
        &Peer {
            public_key: end.main_public_key,
            preshared_key: end.preshared_key,
            description: "main router".to_string(),
            allowed_ip: match end.main_addr {
                IpAddr::V4(_) => "0.0.0.0/0".to_string(),
                IpAddr::V6(_) => "::/0".to_string(),
            },
            endpoint: Some((end.main_endpoint_host, end.main_endpoint_port)),
        },
    )?;
    ensure_zone(cfgs, MAIN_ZONE, FirewallTarget::ACCEPT, MAIN_INTERFACE)?;
    let (section, name) = wg_rule_names(MAIN_INTERFACE);
    ensure_udp_rule(cfgs, &section, &name, BACKHAUL_ZONE, AP_LISTEN_PORT)?;
    set_ap_timeserver(cfgs, end.main_addr)?;
    Ok(())
}

/// Removes the access point's end: the tunnel, its zone and listen rule, and
/// the main router as its time source. The base layout stays.
pub fn remove_ap_end(cfgs: &mut Configs) -> Result<(), Error> {
    let peer_type = format!("wireguard_{MAIN_INTERFACE}");
    cfgs["network"].sections.retain(|s| {
        s.ty() != peer_type
            && !(s.ty() == "interface" && s.name().as_deref() == Some(MAIN_INTERFACE))
    });
    let (_, rule_name) = wg_rule_names(MAIN_INTERFACE);
    cfgs["firewall"]
        .sections
        .retain(|s| !s.get::<FirewallRule>().is_ok_and(|r| r.name == rule_name));
    remove_zone(cfgs, MAIN_ZONE);
    set_timeserver(cfgs, |t| {
        t.server.clear();
        t.enabled = Some(false);
    })
}

fn set_timeserver(cfgs: &mut Configs, edit: impl FnOnce(&mut Timeserver)) -> Result<(), Error> {
    for section in &mut cfgs["system"].sections {
        let Ok(mut ts) = section.get::<Timeserver>() else {
            continue;
        };
        edit(&mut ts);
        section.set(&ts)?;
        return Ok(());
    }
    let mut ts = Timeserver::default();
    edit(&mut ts);
    cfgs["system"].append(&ts, Some("ntp"))?;
    Ok(())
}

/// The main router answers NTP on every management tunnel.
pub fn ensure_main_ntp_server(cfgs: &mut Configs) -> Result<(), Error> {
    set_timeserver(cfgs, |t| {
        t.enabled = Some(true);
        t.enable_server = Some(true);
    })
}

/// Boot heal on a main router with access points: the NTP server, which a main
/// router adopted under an earlier build lacks. Returns
/// whether it wrote, which calls for a firewall reload and an NTP restart.
pub async fn heal_main(uci_root: impl AsRef<std::path::Path>) -> Result<bool, Error> {
    use uciedit::{dump_all, parse_all};

    if crate::access_points::load_registry().is_empty() {
        return Ok(false);
    }
    let arena = Arena::new();
    let mut cfgs = parse_all(uci_root.as_ref(), &arena, &["firewall", "system"]).await?;
    let before = (
        cfgs["firewall"].sections.len(),
        cfgs["system"]
            .sections
            .iter()
            .find_map(|s| s.get::<Timeserver>().ok())
            .and_then(|t| t.enable_server),
    );
    ensure_main_ntp_server(&mut cfgs)?;
    let after = (cfgs["firewall"].sections.len(), Some(true));
    if before == after {
        return Ok(false);
    }
    dump_all(uci_root.as_ref(), cfgs).await?;
    Ok(true)
}

/// The access point takes its time from the main router alone.
fn set_ap_timeserver(cfgs: &mut Configs, main_addr: IpAddr) -> Result<(), Error> {
    set_timeserver(cfgs, |t| {
        t.enabled = Some(true);
        t.enable_server = Some(false);
        t.server = vec![main_addr.to_string()];
        t.use_dhcp = Some(false);
    })
}

#[cfg(test)]
mod tests {
    use uciedit::parse_all;

    use super::*;

    async fn fixture(dir: &std::path::Path) {
        tokio::fs::write(dir.join("network"), "").await.unwrap();
        tokio::fs::write(dir.join("firewall"), "").await.unwrap();
        tokio::fs::write(
            dir.join("system"),
            "config timeserver 'ntp'\n\toption enabled '1'\n\toption enable_server '0'\n\
             \tlist server '0.openwrt.pool.ntp.org'\n",
        )
        .await
        .unwrap();
    }

    const ALL: &[&str] = &["network", "firewall", "system"];

    fn timeserver(cfgs: &Configs) -> Timeserver {
        cfgs["system"]
            .sections
            .iter()
            .find_map(|s| s.get::<Timeserver>().ok())
            .unwrap()
    }

    fn main_end() -> MainEnd<'static> {
        MainEnd {
            access_point: "s1",
            main_addr: "10.42.1.1".parse().unwrap(),
            access_point_addr: "10.42.1.2".parse().unwrap(),
            access_point_public_key: "APKEY=",
            preshared_key: "PSK=",
            listen_port: 51900,
            main_private_key: "MAINPRIV=",
        }
    }

    #[tokio::test]
    async fn main_end_is_idempotent_and_removable() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path()).await;
        let arena = Arena::new();
        let mut cfgs = parse_all(dir.path(), &arena, ALL).await.unwrap();
        provision_main_end(&mut cfgs, &arena, &main_end()).unwrap();
        provision_main_end(&mut cfgs, &arena, &main_end()).unwrap();
        ensure_main_ntp_server(&mut cfgs).unwrap();
        let ts = timeserver(&cfgs);
        assert_eq!(ts.enable_server, Some(true));
        assert_eq!(ts.server.len(), 1, "the main router keeps its own sources");
        let ifaces = cfgs["network"]
            .sections
            .iter()
            .filter(|s| s.name().as_deref() == Some("ap_s1"))
            .count();
        let peers = cfgs["network"]
            .sections
            .iter()
            .filter(|s| s.ty() == "wireguard_ap_s1")
            .count();
        assert_eq!((ifaces, peers), (1, 1));
        let wg = cfgs["network"]
            .sections
            .iter()
            .find(|s| s.name().as_deref() == Some("ap_s1"))
            .unwrap()
            .get::<WgInterface>()
            .unwrap();
        assert_eq!(wg.listen_port, Some(51900));
        assert_eq!(wg.addresses, vec!["10.42.1.1/24"]);
        let zone = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<uciedit::openwrt::FirewallZone>().ok())
            .find(|z| z.name == AP_ZONE)
            .unwrap();
        assert_eq!(zone.network, vec!["ap_s1"]);
        let rule = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<uciedit::openwrt::FirewallRule>().ok())
            .find(|r| r.name == "Allow-WireGuard-ap_s1")
            .unwrap();
        assert_eq!(
            (rule.src.as_str(), rule.dest_port.as_deref()),
            (BACKHAUL_ZONE, Some("51900"))
        );

        remove_main_end(&mut cfgs, "s1").unwrap();
        assert!(cfgs["network"]
            .sections
            .iter()
            .all(|s| s.name().as_deref() != Some("ap_s1") && s.ty() != "wireguard_ap_s1"));
        let zone = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<uciedit::openwrt::FirewallZone>().ok())
            .find(|z| z.name == AP_ZONE)
            .unwrap();
        assert!(zone.network.is_empty());
    }

    #[tokio::test]
    async fn ap_end_dials_a_scoped_endpoint_without_a_host_route() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path()).await;
        let arena = Arena::new();
        let mut cfgs = parse_all(dir.path(), &arena, ALL).await.unwrap();
        let end = ApEnd {
            access_point_addr: "10.42.1.2".parse().unwrap(),
            main_addr: "10.42.1.1".parse().unwrap(),
            main_public_key: "MAINKEY=",
            main_endpoint_host: "fe80::1%br-lan.4094",
            main_endpoint_port: 51900,
            preshared_key: "PSK=",
            private_key: "APPRIV=",
        };
        provision_ap_end(&mut cfgs, &arena, &end).unwrap();
        let wg = cfgs["network"]
            .sections
            .iter()
            .find(|s| s.name().as_deref() == Some(MAIN_INTERFACE))
            .unwrap()
            .get::<WgInterface>()
            .unwrap();
        assert_eq!(wg.nohostroute, Some(true));
        assert_eq!(wg.listen_port, Some(AP_LISTEN_PORT));
        assert_eq!(wg.dns, vec!["10.42.1.1"]);
        let peer = cfgs["network"]
            .sections
            .iter()
            .find(|s| s.ty() == "wireguard_main")
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
            text.contains("allowed_ips") && text.contains("0.0.0.0/0"),
            "{text}"
        );
        assert!(cfgs["firewall"].sections.iter().any(|s| {
            s.get::<FirewallRule>().is_ok_and(|r| {
                r.name == "Allow-WireGuard-main"
                    && r.src == BACKHAUL_ZONE
                    && r.dest_port.as_deref() == Some("51899")
            })
        }));
        let ts = timeserver(&cfgs);
        assert_eq!(ts.server, vec!["10.42.1.1"]);
        assert_eq!(ts.use_dhcp, Some(false));

        remove_ap_end(&mut cfgs).unwrap();
        assert!(cfgs["network"]
            .sections
            .iter()
            .all(|s| s.name().as_deref() != Some(MAIN_INTERFACE) && s.ty() != "wireguard_main"));
        assert!(cfgs["firewall"].sections.iter().all(|s| {
            !s.get::<FirewallRule>()
                .is_ok_and(|r| r.name == "Allow-WireGuard-main")
                && !s
                    .get::<uciedit::openwrt::FirewallZone>()
                    .is_ok_and(|z| z.name == MAIN_ZONE)
        }));
        assert!(timeserver(&cfgs).server.is_empty());
    }
}

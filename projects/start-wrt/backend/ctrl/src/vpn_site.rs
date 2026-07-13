//! Site-to-site WireGuard for satellite routers (Core side).
//!
//! Inbound VPN (`vpn_server.rs`) accepts **host** peers (`/32`) that live inside a
//! profile's own `/24`. A satellite is different: it is a *router* that advertises
//! its **whole per-profile `/24`** over the tunnel (site-to-site). This module
//! generates the Core-side config for one such per-profile tunnel — the design's
//! Option A (one L3 tunnel per profile per satellite), where each tunnel joins its
//! profile's firewall zone so all existing per-profile policy applies unchanged.
//!
//! What this module does (config generation, unit-testable):
//! - a `wg_*` interface carrying the tunnel (transit `/32` on the interface; peer
//!   reachability is via `allowed_ips` cryptokey routing, not the interface subnet);
//! - a peer whose `allowed_ips` is the satellite's **subnet** with
//!   `route_allowed_ips=1` (so the kernel installs the `/24` route via the tunnel);
//! - membership of the tunnel interface in the profile's `vlan_<iface>` zone
//!   (reusing `vpn_server::ensure_firewall_zone`);
//! - an accept rule for the handshake on the **transit-link** zone (not `wan`).
//!
//! Deliberately NOT here yet (see `docs/design/satellite-router-next-steps.md`):
//! transit-subnet/listen-port allocation, key generation & pairing, the WAN-less
//! endpoint pinning on the satellite side, MSS clamp, and the routed-attachment
//! source-rule for VPN-routed profiles in `profiles.rs`. `provision_core_site_tunnel`
//! is a pure config-writer; nothing calls it into effect yet.

use std::net::Ipv4Addr;

use uciedit::openwrt::{
    Dhcp, InterfaceProto, NetworkBridgeVlan, NetworkInterface, NetworkVlanPort,
    NetworkVlanPortTagging,
};
use uciedit::{Arena, Configs, Line, LineComment, Section, Token, TypedSection};

use crate::prelude::*;
use crate::vpn_server::{
    ensure_firewall_zone, ensure_wireguard_firewall_rule_in_zone, WgInterface,
};

/// Satellite per-profile tunnel metadata, stored in `/etc/config/startwrt`.
#[derive(Debug, TypedSection)]
#[uci(ty = "vpn_site")]
pub struct UciVpnSite {
    /// The Core-side WireGuard interface name (e.g. "sat_s1_guest").
    pub interface: String,
    /// The profile this tunnel carries (e.g. "guest").
    pub profile_interface: String,
    /// The satellite this tunnel belongs to (its registry name).
    pub satellite: String,
    /// The satellite's downstream subnet for this profile (CIDR, e.g. "192.168.130.0/24").
    pub subnet: String,
    /// UDP listen port on the Core for this tunnel.
    pub listen_port: u16,
}

/// Inputs for one Core-side per-profile satellite tunnel. Addresses/keys/ports are
/// allocated by the (not-yet-built) pairing flow; this writer just consumes them.
pub struct SiteTunnelParams<'p> {
    /// Satellite registry name.
    pub satellite: &'p str,
    /// Profile interface name the tunnel carries.
    pub profile_interface: &'p str,
    /// The Core end's transit address on this tunnel (an underlay `/32` on the wg iface).
    pub core_transit_addr: Ipv4Addr,
    /// The satellite's downstream subnet carried over the tunnel (CIDR "a.b.c.0/24").
    pub satellite_subnet: &'p str,
    /// The satellite's WireGuard public key (base64), pinned at pairing.
    pub satellite_public_key: &'p str,
    /// Pre-shared key (base64).
    pub preshared_key: &'p str,
    /// UDP listen port on the Core.
    pub listen_port: u16,
    /// The firewall zone the handshake arrives on (the transit-link zone).
    pub transit_zone: &'p str,
    /// The Core's WireGuard private key for this interface (base64).
    pub core_private_key: &'p str,
}

/// The Core-side WireGuard interface name for a satellite's per-profile tunnel.
/// Callers must keep satellite/profile ids short (Linux `IFNAMSIZ` is 15 chars).
pub fn site_interface_name(satellite: &str, profile_interface: &str) -> String {
    format!("sat_{satellite}_{profile_interface}")
}

/// Write the full Core-side config for one per-profile satellite tunnel. Idempotent:
/// re-provisioning updates the interface/metadata and replaces the single peer.
pub fn provision_core_site_tunnel<'a>(
    cfgs: &mut Configs<'a>,
    arena: &'a Arena,
    params: &SiteTunnelParams,
) -> Result<(), Error> {
    let iface = site_interface_name(params.satellite, params.profile_interface);
    set_site_interface(cfgs, &iface, params)?;
    set_site_metadata(cfgs, &iface, params)?;
    add_site_peer(cfgs, &iface, params, arena)?;
    // Attach the tunnel to the profile's zone so all per-profile policy applies to
    // the satellite's routed subnet exactly as it would to a local client.
    ensure_firewall_zone(cfgs, &iface, params.profile_interface)?;
    // Admit the handshake on the transit-link zone (a satellite dials over the local
    // link, not the WAN).
    ensure_wireguard_firewall_rule_in_zone(cfgs, &iface, params.listen_port, params.transit_zone)?;
    Ok(())
}

fn set_site_interface(
    cfgs: &mut Configs,
    interface_name: &str,
    params: &SiteTunnelParams,
) -> Result<(), Error> {
    // Transit /32 on the interface; the satellite subnet is reached via the peer's
    // allowed_ips (cryptokey routing), not the interface address.
    let addresses = vec![format!("{}/32", params.core_transit_addr)];

    for section in &mut cfgs["network"].sections {
        if section.name().as_deref() != Some(interface_name) {
            continue;
        }
        let Ok(mut wg) = section.get::<WgInterface>() else {
            continue;
        };
        if !wg.is_wireguard() {
            continue;
        }
        wg.private_key = params.core_private_key.to_string();
        wg.listen_port = Some(params.listen_port);
        wg.addresses = addresses.clone();
        section.set(&wg)?;
        return Ok(());
    }

    let new_iface = WgInterface {
        proto: "wireguard".to_string(),
        private_key: params.core_private_key.to_string(),
        listen_port: Some(params.listen_port),
        addresses,
        disabled: None,
        mtu: None,
    };
    cfgs["network"].append(&new_iface, Some(interface_name))?;
    Ok(())
}

fn set_site_metadata(
    cfgs: &mut Configs,
    interface_name: &str,
    params: &SiteTunnelParams,
) -> Result<(), Error> {
    for section in &mut cfgs["startwrt"].sections {
        let Ok(mut meta) = section.get::<UciVpnSite>() else {
            continue;
        };
        if meta.interface != interface_name {
            continue;
        }
        meta.profile_interface = params.profile_interface.to_string();
        meta.satellite = params.satellite.to_string();
        meta.subnet = params.satellite_subnet.to_string();
        meta.listen_port = params.listen_port;
        section.set(&meta)?;
        return Ok(());
    }

    let meta = UciVpnSite {
        interface: interface_name.to_string(),
        profile_interface: params.profile_interface.to_string(),
        satellite: params.satellite.to_string(),
        subnet: params.satellite_subnet.to_string(),
        listen_port: params.listen_port,
    };
    cfgs["startwrt"].append(&meta, Some(interface_name))?;
    Ok(())
}

fn add_site_peer<'a>(
    cfgs: &mut Configs<'a>,
    interface_name: &str,
    params: &SiteTunnelParams,
    arena: &'a Arena,
) -> Result<(), Error> {
    let peer_type = format!("wireguard_{interface_name}");
    let peer_type_str: &str = arena.alloc(peer_type);

    // Exactly one peer per site tunnel — drop any prior one so re-provision is idempotent.
    cfgs["network"].sections.retain(|s| s.ty() != peer_type_str);

    let peer_name_str: &str = arena.alloc(format!("{interface_name}_peer"));

    let mut lines = vec![Line::Section {
        ty: Token::from_str(peer_type_str, arena),
        name: Some(Token::from_str(peer_name_str, arena)),
        comment: LineComment::None,
    }];

    let pub_key_str: &str = arena.alloc(params.satellite_public_key.to_string());
    lines.push(Line::Option {
        option: Token::from_str("public_key", arena),
        value: Token::from_str(pub_key_str, arena),
        comment: LineComment::None,
    });

    let desc_str: &str =
        arena.alloc(format!("satellite {} ({})", params.satellite, params.profile_interface));
    lines.push(Line::Option {
        option: Token::from_str("description", arena),
        value: Token::from_str(desc_str, arena),
        comment: LineComment::None,
    });

    let psk_str: &str = arena.alloc(params.preshared_key.to_string());
    lines.push(Line::Option {
        option: Token::from_str("preshared_key", arena),
        value: Token::from_str(psk_str, arena),
        comment: LineComment::None,
    });

    lines.push(Line::Option {
        option: Token::from_str("persistent_keepalive", arena),
        value: Token::from_str("25", arena),
        comment: LineComment::None,
    });

    // Install the downstream subnet route via this interface.
    lines.push(Line::Option {
        option: Token::from_str("route_allowed_ips", arena),
        value: Token::from_str("1", arena),
        comment: LineComment::None,
    });

    // Site-to-site: the satellite advertises its whole per-profile subnet, NOT a /32 host.
    let subnet_str: &str = arena.alloc(params.satellite_subnet.to_string());
    lines.push(Line::List {
        list: Token::from_str("allowed_ips", arena),
        item: Token::from_str(subnet_str, arena),
        comment: LineComment::None,
    });

    cfgs["network"].sections.push(Section { arena, lines });
    Ok(())
}

// ── Satellite side (dials the Core) ─────────────────────────────────────────

/// Inputs for the satellite end of a per-profile tunnel. The satellite dials the
/// Core over the local transit link, then routes `allowed_ips` (e.g. `0.0.0.0/0`
/// for full egress-via-Core, since a satellite has no WAN) through the tunnel.
pub struct SatelliteTunnelParams<'p> {
    pub satellite: &'p str,
    pub profile_interface: &'p str,
    /// The satellite end's WireGuard address (a host in the profile `/24`).
    pub sat_wg_addr: Ipv4Addr,
    /// The Core's WireGuard public key.
    pub core_public_key: &'p str,
    /// The Core's underlay endpoint host to dial (its transit address).
    pub core_endpoint_host: &'p str,
    /// The Core's listen port for this tunnel.
    pub core_endpoint_port: u16,
    pub preshared_key: &'p str,
    pub sat_private_key: &'p str,
    /// CIDRs to route through the tunnel. Full egress via the Core: `["0.0.0.0/0"]`.
    pub allowed_ips: &'p [String],
    /// A local interface whose firewall zone the tunnel joins (e.g. "lan"), so
    /// replies to the satellite and forwarding from its LAN are permitted.
    pub firewall_zone_member: &'p str,
}

/// Write the satellite-side config for one per-profile tunnel. Idempotent.
pub fn provision_satellite_site_tunnel<'a>(
    cfgs: &mut Configs<'a>,
    arena: &'a Arena,
    params: &SatelliteTunnelParams,
) -> Result<(), Error> {
    let iface = site_interface_name(params.satellite, params.profile_interface);
    set_sat_interface(cfgs, &iface, params)?;
    add_sat_peer(cfgs, &iface, params, arena)?;
    ensure_firewall_zone(cfgs, &iface, params.firewall_zone_member)?;
    Ok(())
}

fn set_sat_interface(
    cfgs: &mut Configs,
    interface_name: &str,
    params: &SatelliteTunnelParams,
) -> Result<(), Error> {
    let addresses = vec![format!("{}/32", params.sat_wg_addr)];
    for section in &mut cfgs["network"].sections {
        if section.name().as_deref() != Some(interface_name) {
            continue;
        }
        let Ok(mut wg) = section.get::<WgInterface>() else {
            continue;
        };
        if !wg.is_wireguard() {
            continue;
        }
        wg.private_key = params.sat_private_key.to_string();
        wg.listen_port = None; // the satellite dials out; it does not listen
        wg.addresses = addresses.clone();
        section.set(&wg)?;
        return Ok(());
    }
    let new_iface = WgInterface {
        proto: "wireguard".to_string(),
        private_key: params.sat_private_key.to_string(),
        listen_port: None,
        addresses,
        disabled: None,
        mtu: None,
    };
    cfgs["network"].append(&new_iface, Some(interface_name))?;
    Ok(())
}

fn add_sat_peer<'a>(
    cfgs: &mut Configs<'a>,
    interface_name: &str,
    params: &SatelliteTunnelParams,
    arena: &'a Arena,
) -> Result<(), Error> {
    let peer_type = format!("wireguard_{interface_name}");
    let peer_type_str: &str = arena.alloc(peer_type);
    cfgs["network"].sections.retain(|s| s.ty() != peer_type_str);

    let peer_name_str: &str = arena.alloc(format!("{interface_name}_peer"));
    let mut lines = vec![Line::Section {
        ty: Token::from_str(peer_type_str, arena),
        name: Some(Token::from_str(peer_name_str, arena)),
        comment: LineComment::None,
    }];

    let pub_key_str: &str = arena.alloc(params.core_public_key.to_string());
    lines.push(Line::Option {
        option: Token::from_str("public_key", arena),
        value: Token::from_str(pub_key_str, arena),
        comment: LineComment::None,
    });

    let desc_str: &str = arena.alloc(format!("core ({})", params.profile_interface));
    lines.push(Line::Option {
        option: Token::from_str("description", arena),
        value: Token::from_str(desc_str, arena),
        comment: LineComment::None,
    });

    let psk_str: &str = arena.alloc(params.preshared_key.to_string());
    lines.push(Line::Option {
        option: Token::from_str("preshared_key", arena),
        value: Token::from_str(psk_str, arena),
        comment: LineComment::None,
    });

    // Endpoint to dial (OpenWrt wireguard peer options).
    let host_str: &str = arena.alloc(params.core_endpoint_host.to_string());
    lines.push(Line::Option {
        option: Token::from_str("endpoint_host", arena),
        value: Token::from_str(host_str, arena),
        comment: LineComment::None,
    });
    let port_str: &str = arena.alloc(params.core_endpoint_port.to_string());
    lines.push(Line::Option {
        option: Token::from_str("endpoint_port", arena),
        value: Token::from_str(port_str, arena),
        comment: LineComment::None,
    });

    lines.push(Line::Option {
        option: Token::from_str("persistent_keepalive", arena),
        value: Token::from_str("25", arena),
        comment: LineComment::None,
    });

    // Install routes for the tunneled prefixes (the satellite's uplink).
    lines.push(Line::Option {
        option: Token::from_str("route_allowed_ips", arena),
        value: Token::from_str("1", arena),
        comment: LineComment::None,
    });

    for cidr in params.allowed_ips {
        let cidr_str: &str = arena.alloc(cidr.clone());
        lines.push(Line::List {
            list: Token::from_str("allowed_ips", arena),
            item: Token::from_str(cidr_str, arena),
            comment: LineComment::None,
        });
    }

    cfgs["network"].sections.push(Section { arena, lines });
    Ok(())
}

// ── Satellite local profile network (so a downstream client lands on the profile) ──

/// Inputs for a satellite's LOCAL serving of one profile: a VLAN interface with
/// the profile's satellite-local `/24`, a DHCP pool, a LAN port assigned to the
/// VLAN, and firewall-zone membership. A client on that port then routes out the
/// tunnel to the Core (which applies the profile's policy). This is a manual
/// stand-in for what the config-sync flow (D5) will do automatically.
pub struct SatelliteLocalProfileParams<'p> {
    /// The network interface name to create on the satellite (e.g. "psat_guest").
    pub profile_interface: &'p str,
    pub vlan_tag: u16,
    /// The satellite's gateway address for this profile `/24` (its `.1`).
    pub gateway: Ipv4Addr,
    /// A satellite LAN port to place on this profile's VLAN (untagged).
    pub port: &'p str,
    /// A local interface whose firewall zone this profile joins (same as the
    /// tunnel's, so intra-zone forwarding + the default route via the tunnel egress).
    pub firewall_zone_member: &'p str,
}

/// Write the satellite-local serving config for one profile. Idempotent.
pub fn provision_satellite_local_profile(
    cfgs: &mut Configs,
    params: &SatelliteLocalProfileParams,
) -> Result<(), Error> {
    set_local_profile_interface(cfgs, params)?;
    set_local_profile_bridge_vlan(cfgs, params)?;
    set_local_profile_dhcp(cfgs, params)?;
    ensure_firewall_zone(cfgs, params.profile_interface, params.firewall_zone_member)?;
    Ok(())
}

fn set_local_profile_interface(
    cfgs: &mut Configs,
    params: &SatelliteLocalProfileParams,
) -> Result<(), Error> {
    let device = format!("br-lan.{}", params.vlan_tag);
    let netmask = Ipv4Addr::new(255, 255, 255, 0);
    for section in &mut cfgs["network"].sections {
        if section.name().as_deref() != Some(params.profile_interface) {
            continue;
        }
        if let Ok(mut ni) = section.get::<NetworkInterface>() {
            ni.device = device.clone();
            ni.proto = InterfaceProto::STATIC;
            ni.ipaddr = Some(params.gateway);
            ni.netmask = Some(netmask);
            section.set(&ni)?;
            return Ok(());
        }
    }
    let ni = NetworkInterface {
        device,
        proto: InterfaceProto::STATIC,
        ipaddr: Some(params.gateway),
        netmask: Some(netmask),
        ..Default::default()
    };
    cfgs["network"].append(&ni, Some(params.profile_interface))?;
    Ok(())
}

fn set_local_profile_bridge_vlan(
    cfgs: &mut Configs,
    params: &SatelliteLocalProfileParams,
) -> Result<(), Error> {
    for section in &mut cfgs["network"].sections {
        if let Ok(mut bv) = section.get::<NetworkBridgeVlan>() {
            if bv.device == "br-lan" && bv.vlan == params.vlan_tag {
                if !bv.ports.iter().any(|p| p.port == params.port) {
                    bv.ports.push(NetworkVlanPort {
                        port: params.port.to_string(),
                        tagging: Some(NetworkVlanPortTagging::PRIMARY),
                    });
                    section.set(&bv)?;
                }
                return Ok(());
            }
        }
    }
    let bv = NetworkBridgeVlan {
        device: "br-lan".to_string(),
        vlan: params.vlan_tag,
        ports: vec![NetworkVlanPort {
            port: params.port.to_string(),
            tagging: Some(NetworkVlanPortTagging::PRIMARY),
        }],
    };
    let section_name = format!("bv_{}", params.vlan_tag);
    cfgs["network"].append(&bv, Some(section_name.as_str()))?;
    Ok(())
}

fn set_local_profile_dhcp(
    cfgs: &mut Configs,
    params: &SatelliteLocalProfileParams,
) -> Result<(), Error> {
    for section in &mut cfgs["dhcp"].sections {
        if section.name().as_deref() != Some(params.profile_interface) {
            continue;
        }
        if let Ok(mut d) = section.get::<Dhcp>() {
            d.interface = params.profile_interface.to_string();
            d.start = 2;
            d.limit = 200;
            d.leasetime = "12h".to_string();
            section.set(&d)?;
            return Ok(());
        }
    }
    let d = Dhcp {
        interface: params.profile_interface.to_string(),
        start: 2,
        limit: 200,
        leasetime: "12h".to_string(),
        ra: None,
        dhcpv6: None,
        ra_management: None,
        ra_default: None,
    };
    cfgs["dhcp"].append(&d, Some(params.profile_interface))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uciedit::openwrt::{FirewallRule, FirewallZone};
    use uciedit::parse_all;

    fn write_base(dir: &std::path::Path) {
        std::fs::write(
            dir.join("network"),
            "config interface 'guest'\n\toption device 'br-lan.101'\n\toption proto 'static'\n\
             \toption ipaddr '192.168.101.1'\n\toption netmask '255.255.255.0'\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("startwrt"),
            "config profile guest\n\toption fullname 'Guest'\n\toption interface 'guest'\n\
             \toption vlan_tag '101'\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("firewall"),
            "config zone\n\toption name 'vlan_guest'\n\tlist network 'guest'\n\
             \toption input 'ACCEPT'\n\toption output 'ACCEPT'\n\toption forward 'ACCEPT'\n\
             \nconfig zone\n\toption name 'lan'\n\tlist network 'lan'\n\
             \toption input 'ACCEPT'\n\toption output 'ACCEPT'\n\toption forward 'ACCEPT'\n",
        )
        .unwrap();
        std::fs::write(dir.join("dhcp"), "").unwrap();
    }

    fn params<'p>() -> SiteTunnelParams<'p> {
        SiteTunnelParams {
            satellite: "s1",
            profile_interface: "guest",
            core_transit_addr: "10.42.0.1".parse().unwrap(),
            satellite_subnet: "192.168.130.0/24",
            satellite_public_key: "PUBKEY",
            preshared_key: "PSK",
            listen_port: 51900,
            transit_zone: "transit",
            core_private_key: "PRIVKEY",
        }
    }

    #[test]
    fn interface_name_is_prefixed() {
        assert_eq!(site_interface_name("s1", "guest"), "sat_s1_guest");
    }

    #[tokio::test]
    async fn provisions_interface_peer_zone_and_rule() {
        let dir = tempfile::tempdir().unwrap();
        write_base(dir.path());
        let arena = Arena::new();
        let mut cfgs = parse_all(dir.path(), &arena, &["network", "startwrt", "firewall"])
            .await
            .unwrap();

        provision_core_site_tunnel(&mut cfgs, &arena, &params()).unwrap();

        // Interface exists, transit /32, correct listen port.
        let iface = cfgs["network"]
            .sections
            .iter()
            .filter_map(|s| {
                (s.name().as_deref() == Some("sat_s1_guest"))
                    .then(|| s.get::<WgInterface>().ok())
                    .flatten()
            })
            .next()
            .expect("wg interface should exist");
        assert!(iface.addresses.iter().any(|a| a == "10.42.0.1/32"));
        assert_eq!(iface.listen_port, Some(51900));

        // A peer section of the tunnel's type exists (the /24 advertiser).
        assert!(
            cfgs["network"]
                .sections
                .iter()
                .any(|s| s.ty() == "wireguard_sat_s1_guest"),
            "site peer section should exist"
        );

        // The tunnel joined the profile's firewall zone.
        let zone = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<FirewallZone>().ok())
            .find(|z| z.name == "vlan_guest")
            .expect("profile zone should exist");
        assert!(
            zone.network.iter().any(|n| n == "sat_s1_guest"),
            "tunnel should be a member of the profile zone"
        );

        // Handshake accepted on the transit zone (not wan).
        let rule = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<FirewallRule>().ok())
            .find(|r| r.name == "Allow-WireGuard-sat_s1_guest")
            .expect("accept rule should exist");
        assert_eq!(rule.src, "transit");
        assert_eq!(rule.dest_port.as_deref(), Some("51900"));

        // Metadata recorded.
        let meta = cfgs["startwrt"]
            .sections
            .iter()
            .filter_map(|s| s.get::<UciVpnSite>().ok())
            .find(|m| m.interface == "sat_s1_guest")
            .expect("vpn_site metadata should exist");
        assert_eq!(meta.subnet, "192.168.130.0/24");
        assert_eq!(meta.satellite, "s1");
    }

    #[tokio::test]
    async fn reprovision_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        write_base(dir.path());
        let arena = Arena::new();
        let mut cfgs = parse_all(dir.path(), &arena, &["network", "startwrt", "firewall"])
            .await
            .unwrap();

        provision_core_site_tunnel(&mut cfgs, &arena, &params()).unwrap();
        provision_core_site_tunnel(&mut cfgs, &arena, &params()).unwrap();

        let peers = cfgs["network"]
            .sections
            .iter()
            .filter(|s| s.ty() == "wireguard_sat_s1_guest")
            .count();
        assert_eq!(peers, 1, "re-provision must not duplicate the peer");
    }

    fn sat_params<'p>(allowed: &'p [String]) -> SatelliteTunnelParams<'p> {
        SatelliteTunnelParams {
            satellite: "s1",
            profile_interface: "guest",
            sat_wg_addr: "192.168.130.2".parse().unwrap(),
            core_public_key: "COREPUB",
            core_endpoint_host: "10.42.0.1",
            core_endpoint_port: 51900,
            preshared_key: "PSK",
            sat_private_key: "SATPRIV",
            allowed_ips: allowed,
            firewall_zone_member: "lan",
        }
    }

    #[tokio::test]
    async fn provisions_satellite_side_tunnel() {
        let dir = tempfile::tempdir().unwrap();
        write_base(dir.path());
        let arena = Arena::new();
        let mut cfgs = parse_all(dir.path(), &arena, &["network", "startwrt", "firewall"])
            .await
            .unwrap();

        let allowed = vec!["0.0.0.0/0".to_string()];
        provision_satellite_site_tunnel(&mut cfgs, &arena, &sat_params(&allowed)).unwrap();

        // Client interface: no listen_port (it dials), correct wg address.
        let iface = cfgs["network"]
            .sections
            .iter()
            .filter_map(|s| {
                (s.name().as_deref() == Some("sat_s1_guest"))
                    .then(|| s.get::<WgInterface>().ok())
                    .flatten()
            })
            .next()
            .expect("wg interface should exist");
        assert_eq!(iface.listen_port, None);
        assert!(iface.addresses.iter().any(|a| a == "192.168.130.2/32"));

        // Peer exists and the tunnel joined the local lan zone.
        assert!(cfgs["network"]
            .sections
            .iter()
            .any(|s| s.ty() == "wireguard_sat_s1_guest"));
        let zone = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<FirewallZone>().ok())
            .find(|z| z.name == "lan")
            .expect("lan zone should exist");
        assert!(zone.network.iter().any(|n| n == "sat_s1_guest"));
    }

    #[tokio::test]
    async fn provisions_satellite_local_profile_network() {
        let dir = tempfile::tempdir().unwrap();
        write_base(dir.path());
        let arena = Arena::new();
        let mut cfgs = parse_all(dir.path(), &arena, &["network", "startwrt", "firewall", "dhcp"])
            .await
            .unwrap();

        let params = SatelliteLocalProfileParams {
            profile_interface: "psat_guest",
            vlan_tag: 130,
            gateway: "192.168.130.1".parse().unwrap(),
            port: "lan2",
            firewall_zone_member: "lan",
        };
        provision_satellite_local_profile(&mut cfgs, &params).unwrap();

        let ni = cfgs["network"]
            .sections
            .iter()
            .filter_map(|s| {
                (s.name().as_deref() == Some("psat_guest"))
                    .then(|| s.get::<NetworkInterface>().ok())
                    .flatten()
            })
            .next()
            .expect("profile interface should exist");
        assert_eq!(ni.device, "br-lan.130");
        assert_eq!(ni.ipaddr, Some("192.168.130.1".parse().unwrap()));

        let bv = cfgs["network"]
            .sections
            .iter()
            .filter_map(|s| s.get::<NetworkBridgeVlan>().ok())
            .find(|b| b.vlan == 130)
            .expect("bridge-vlan should exist");
        assert!(bv.ports.iter().any(|p| p.port == "lan2"));

        let dhcp = cfgs["dhcp"]
            .sections
            .iter()
            .filter_map(|s| s.get::<Dhcp>().ok())
            .find(|d| d.interface == "psat_guest")
            .expect("dhcp pool should exist");
        assert_eq!(dhcp.start, 2);

        let zone = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<FirewallZone>().ok())
            .find(|z| z.name == "lan")
            .expect("lan zone should exist");
        assert!(zone.network.iter().any(|n| n == "psat_guest"));
    }
}

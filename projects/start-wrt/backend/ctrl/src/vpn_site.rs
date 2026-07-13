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
             \toption input 'ACCEPT'\n\toption output 'ACCEPT'\n\toption forward 'ACCEPT'\n",
        )
        .unwrap();
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
}

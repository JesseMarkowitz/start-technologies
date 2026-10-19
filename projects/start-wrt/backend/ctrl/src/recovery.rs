//! Recovery mode (design D22, §19): a dark access point serves one isolated
//! network, on its access ports and, with a sticker password to protect it,
//! on a 2.4 GHz SSID of its own, where `router.lan` is a page that says why
//! it is dark and offers a restart and a factory reset. The network reaches
//! that page and nothing else.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use uciedit::openwrt::{
    Dhcp, FirewallTarget, InterfaceProto, NetworkBridgeVlan, NetworkInterface, ProfileDnsmasq,
    WifiDevice, WifiInterface, WifiMode,
};
use uciedit::{dump_all, parse_all, Arena, Configs};

use crate::ap_page::esc;
use crate::backhaul::{ensure_zone, primary, remove_zone, tagged, BACKHAUL_VLAN};
use crate::prelude::*;
use crate::satellite;

/// Dark this long before the recovery network comes up.
pub const AFTER_DARK: Duration = Duration::from_secs(60);
pub const VLAN: u16 = 4093;
pub const INTERFACE: &str = "recovery";
pub const ZONE: &str = "recovery";
const WIFI_SECTION: &str = "recovery";
pub const ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 255, 1);
const NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);
const ROUTER_LAN: &str = "/router.lan/192.168.255.1";
const SSID_PREFIX: &str = "StartWRT-Recovery-";
const LOG_LINES: usize = 200;

static ACTIVE: Mutex<Option<tokio::task::AbortHandle>> = Mutex::new(None);

pub fn active() -> bool {
    ACTIVE.lock().unwrap().is_some()
}

/// `StartWRT-Recovery-<last four hex digits of the uplink's MAC>`.
pub fn ssid(uplink: &str) -> String {
    let mac =
        std::fs::read_to_string(format!("/sys/class/net/{uplink}/address")).unwrap_or_default();
    ssid_for_mac(mac.trim())
}

pub fn ssid_for_mac(mac: &str) -> String {
    let hex: String = mac
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_uppercase();
    let suffix = if hex.len() >= 4 {
        hex[hex.len() - 4..].to_string()
    } else {
        "0000".to_string()
    };
    format!("{SSID_PREFIX}{suffix}")
}

/// A recovery SSID needs the sticker password; a board without one has
/// recovery over its access ports only.
pub fn wifi_available() -> bool {
    crate::eeprom::read_wifi_password().ok().flatten().is_some()
}

/// The 2.4 GHz-capable radio to carry the recovery SSID, by section name.
fn recovery_radio(cfgs: &Configs) -> Option<String> {
    cfgs["wireless"]
        .sections
        .iter()
        .filter_map(|s| Some((s.name()?.to_string(), s.get::<WifiDevice>().ok()?)))
        .find(|(_, d)| d.band == "2g")
        .map(|(n, _)| n)
}

/// Writes the recovery network over the access point's layout: access ports
/// move to the recovery VLAN, an addressed interface with DHCP and `router.lan`
/// appears, profile SSIDs are disabled and the recovery SSID added.
pub fn write_layout(cfgs: &mut Configs, uplink: &str, sticker: Option<&str>) -> Result<(), Error> {
    let bridge = crate::ethernet::find_lan_bridge(cfgs)?
        .map(|d| d.name)
        .unwrap_or_else(|| crate::ethernet::DEFAULT_LAN_BRIDGE.to_string());
    let access: BTreeSet<String> = cfgs["network"]
        .sections
        .iter()
        .filter_map(|s| s.get::<uciedit::openwrt::NetworkDevice>().ok())
        .filter(|d| d.name == bridge)
        .flat_map(|d| d.ports)
        .filter(|p| p != uplink && !p.starts_with("wlan") && !p.starts_with("phy"))
        .collect();
    // Access ports leave every profile VLAN while dark.
    for section in &mut cfgs["network"].sections {
        let Ok(mut vlan) = section.get::<NetworkBridgeVlan>() else {
            continue;
        };
        if vlan.device != bridge || vlan.vlan == BACKHAUL_VLAN {
            continue;
        }
        let before = vlan.ports.len();
        vlan.ports.retain(|p| !access.contains(&p.port));
        if vlan.ports.len() != before {
            section.set(&vlan)?;
        }
    }
    cfgs["network"].sections.retain(|s| {
        !s.get::<NetworkBridgeVlan>()
            .is_ok_and(|v| v.device == bridge && v.vlan == VLAN)
    });
    let mut ports: Vec<_> = access.iter().map(|p| primary(p)).collect();
    ports.push(tagged(uplink));
    ports.retain(|p| p.port != uplink);
    cfgs["network"].append(
        &NetworkBridgeVlan {
            device: bridge.clone(),
            vlan: VLAN,
            ports,
        },
        None,
    )?;
    cfgs["network"]
        .sections
        .retain(|s| !(s.ty() == "interface" && s.name().as_deref() == Some(INTERFACE)));
    cfgs["network"].append(
        &NetworkInterface {
            device: format!("{bridge}.{VLAN}"),
            proto: InterfaceProto::STATIC,
            ipaddr: Some(ADDRESS),
            netmask: Some(NETMASK),
            ..Default::default()
        },
        Some(INTERFACE),
    )?;
    cfgs["dhcp"]
        .sections
        .retain(|s| !(s.ty() == "dhcp" && s.name().as_deref() == Some(INTERFACE)));
    cfgs["dhcp"].append(
        &Dhcp {
            interface: INTERFACE.to_string(),
            start: 100,
            limit: 50,
            leasetime: "1h".to_string(),
            ra: None,
            dhcpv6: None,
            ra_management: None,
            ra_default: None,
        },
        Some(INTERFACE),
    )?;
    for section in &mut cfgs["dhcp"].sections {
        let Ok(mut dnsmasq) = section.get::<ProfileDnsmasq>() else {
            continue;
        };
        if !dnsmasq.address.iter().any(|a| a == ROUTER_LAN) {
            dnsmasq.address.push(ROUTER_LAN.to_string());
            section.set(&dnsmasq)?;
        }
        break;
    }
    ensure_zone(cfgs, ZONE, FirewallTarget::ACCEPT, INTERFACE)?;
    for section in &mut cfgs["wireless"].sections {
        let Some(mut iface) = section.get_typed::<WifiInterface>()? else {
            continue;
        };
        if section.name().as_deref() == Some(WIFI_SECTION) {
            continue;
        }
        iface.disabled = Some(true);
        section.set(&iface)?;
    }
    cfgs["wireless"]
        .sections
        .retain(|s| s.name().as_deref() != Some(WIFI_SECTION));
    if let (Some(key), Some(radio)) = (sticker, recovery_radio(cfgs)) {
        cfgs["wireless"].append(
            &WifiInterface {
                device: radio,
                mode: WifiMode::AP,
                ssid: ssid(uplink),
                hidden: false,
                encryption: "psk2".to_string(),
                key: Some(key.to_string()),
                dynamic_vlan: Default::default(),
                maxassoc: Some(1),
                network: Some(INTERFACE.to_string()),
                disabled: Some(false),
            },
            Some(WIFI_SECTION),
        )?;
    }
    Ok(())
}

/// Removes the recovery network and restores the applied layout.
pub fn remove_layout(cfgs: &mut Configs, uplink: &str) -> Result<(), Error> {
    let bridge = crate::ethernet::find_lan_bridge(cfgs)?
        .map(|d| d.name)
        .unwrap_or_else(|| crate::ethernet::DEFAULT_LAN_BRIDGE.to_string());
    cfgs["network"].sections.retain(|s| {
        !s.get::<NetworkBridgeVlan>()
            .is_ok_and(|v| v.device == bridge && v.vlan == VLAN)
    });
    cfgs["network"]
        .sections
        .retain(|s| !(s.ty() == "interface" && s.name().as_deref() == Some(INTERFACE)));
    cfgs["dhcp"]
        .sections
        .retain(|s| !(s.ty() == "dhcp" && s.name().as_deref() == Some(INTERFACE)));
    for section in &mut cfgs["dhcp"].sections {
        let Ok(mut dnsmasq) = section.get::<ProfileDnsmasq>() else {
            continue;
        };
        if dnsmasq.address.iter().any(|a| a == ROUTER_LAN) {
            dnsmasq.address.retain(|a| a != ROUTER_LAN);
            section.set(&dnsmasq)?;
        }
    }
    remove_zone(cfgs, ZONE);
    cfgs["wireless"]
        .sections
        .retain(|s| s.name().as_deref() != Some(WIFI_SECTION));
    for section in &mut cfgs["wireless"].sections {
        let Some(mut iface) = section.get_typed::<WifiInterface>()? else {
            continue;
        };
        if iface.disabled == Some(true) {
            iface.disabled = None;
            section.set(&iface)?;
        }
    }
    match crate::ap::load_applied() {
        Some(applied) => crate::ap::write_snapshot(cfgs, &applied, uplink)?,
        None => crate::ap::write_base_layout(cfgs, uplink)?,
    }
    Ok(())
}

const CONFIGS: &[&str] = &["network", "firewall", "dhcp", "wireless"];

/// Brings the recovery network up on a dark access point.
pub async fn enter(uplink: &str) -> Result<(), Error> {
    if active() {
        return Ok(());
    }
    let sticker = crate::eeprom::read_wifi_password().ok().flatten();
    {
        let arena = Arena::new();
        let mut cfgs = parse_all("/etc/config", &arena, CONFIGS).await?;
        write_layout(&mut cfgs, uplink, sticker.as_deref())?;
        dump_all("/etc/config", cfgs).await?;
    }
    crate::ap::reload_all().await;
    for port in crate::liveness::access_ports(uplink) {
        let _ = crate::run_quiet_async(
            tokio::process::Command::new("ip").args(["link", "set", "dev", &port, "up"]),
        )
        .await;
    }
    let handle = tokio::spawn(async {
        if let Err(e) = serve().await {
            tracing::error!("recovery: page not served: {e}");
        }
    });
    *ACTIVE.lock().unwrap() = Some(handle.abort_handle());
    crate::activity::log(
        "satellite",
        "recovery",
        false,
        &format!(
            "Recovery mode on: router.lan on the access ports{}",
            if sticker.is_some() {
                format!(" and on Wi-Fi {}", ssid(uplink))
            } else {
                String::new()
            }
        ),
        None,
    );
    Ok(())
}

/// Takes the recovery network down and restores the applied layout.
pub async fn exit(uplink: &str) -> Result<(), Error> {
    let Some(handle) = ACTIVE.lock().unwrap().take() else {
        return Ok(());
    };
    handle.abort();
    {
        let arena = Arena::new();
        let mut cfgs = parse_all("/etc/config", &arena, CONFIGS).await?;
        remove_layout(&mut cfgs, uplink)?;
        dump_all("/etc/config", cfgs).await?;
    }
    crate::ap::reload_all().await;
    crate::activity::log(
        "satellite",
        "recovery",
        true,
        "Recovery mode off: the Core is back",
        None,
    );
    Ok(())
}

// ── The page ────────────────────────────────────────────────────────────────

/// Why the access point is dark, as far as it can tell.
pub struct Reasons {
    pub uplink: String,
    pub uplink_link: bool,
    pub core_neighbour: bool,
    pub handshake_age: Option<u64>,
    pub version: String,
    pub core_version: String,
    pub refused: Option<String>,
}

async fn reasons(uplink: &str) -> Reasons {
    let role = satellite::load_role();
    let link = std::fs::read_to_string(format!("/sys/class/net/{uplink}/carrier"))
        .is_ok_and(|c| c.trim() == "1");
    let core_neighbour = match role
        .core_endpoint
        .as_deref()
        .and_then(|e| e.split_once('%'))
    {
        Some((ip, dev)) => {
            let out = tokio::process::Command::new("ip")
                .args(["-6", "neigh", "show", "dev", dev])
                .output()
                .await
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            out.lines()
                .any(|l| l.starts_with(ip) && l.contains("lladdr") && !l.contains("FAILED"))
        }
        None => false,
    };
    let handshake_age = tokio::process::Command::new("wg")
        .args([
            "show",
            crate::mgmt_tunnel::AP_INTERFACE,
            "latest-handshakes",
        ])
        .output()
        .await
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .nth(1)?
                .parse::<u64>()
                .ok()
        })
        .filter(|t| *t > 0)
        .map(|t| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().saturating_sub(t))
                .unwrap_or(0)
        });
    Reasons {
        uplink: uplink.to_string(),
        uplink_link: link,
        core_neighbour,
        handshake_age,
        version: env!("CARGO_PKG_VERSION").to_string(),
        core_version: crate::control::core_version(),
        refused: crate::ap::last_apply_error(),
    }
}

/// Log lines about the uplink, the tunnel, the firewall and sync; nothing
/// about clients.
pub fn filter_log(log: &str) -> String {
    const KEEP: &[&str] = &[
        "kern.",
        "netifd",
        "wireguard",
        "firewall",
        "startwrt",
        "sysntpd",
        "ntpd",
    ];
    const DROP: &[&str] = &[
        "dnsmasq-dhcp",
        "hostapd",
        "DHCPACK",
        "DHCPREQUEST",
        "DHCPDISCOVER",
        "DHCPOFFER",
        "odhcpd",
    ];
    log.lines()
        .filter(|l| KEEP.iter().any(|k| l.contains(k)) && !DROP.iter().any(|d| l.contains(d)))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn log_text() -> String {
    let out = tokio::process::Command::new("logread")
        .output()
        .await
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let filtered = filter_log(&out);
    let lines: Vec<&str> = filtered.lines().collect();
    let start = lines.len().saturating_sub(LOG_LINES);
    lines[start..].join("\n")
}

pub fn render(name: &str, r: &Reasons, log: &str) -> String {
    let mut h = String::new();
    let name = esc(name);
    let _ = write!(
        h,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>Satellite {name} — recovery mode</title>\
         <style>body{{font:16px/1.5 system-ui,sans-serif;max-width:40rem;margin:2rem auto;padding:0 1rem;color:#222}}\
         h1{{font-size:1.5rem}}h2{{font-size:1.1rem;margin-top:1.5rem}}table{{border-collapse:collapse;width:100%}}\
         td,th{{text-align:left;padding:.25rem .5rem;border-bottom:1px solid #ddd;vertical-align:top}}\
         pre{{font-size:.75rem;overflow:auto;background:#f4f4f4;padding:.5rem;max-height:24rem}}\
         .muted{{color:#666}}.bad{{color:#b00}}.ok{{color:#070}}form{{display:inline}}\
         button{{font:inherit;padding:.4rem .9rem;margin:.25rem .5rem 0 0}}</style></head><body>\
         <h1>Satellite {name} — recovery mode</h1>\
         <p>This access point has lost its main router, so it serves no network until it is back. \
         Devices near it find their usual network only where the main router or another access point \
         still reaches them.</p>\
         <h2>What it can tell</h2><table>\
         <tr><th>Uplink {uplink}</th><td class=\"{lc}\">{link}</td></tr>\
         <tr><th>Main router on the cable</th><td class=\"{nc}\">{neigh}</td></tr>\
         <tr><th>Management tunnel</th><td class=\"{hc}\">{hs}</td></tr>\
         <tr><th>Firmware</th><td class=\"{vc}\">{ver}</td></tr>\
         <tr><th>Last configuration</th><td class=\"{rc}\">{refused}</td></tr></table>",
        uplink = esc(&r.uplink),
        lc = if r.uplink_link { "ok" } else { "bad" },
        link = if r.uplink_link { "link up" } else { "no link: check the cable to the main router" },
        nc = if r.core_neighbour { "ok" } else { "bad" },
        neigh = if r.core_neighbour { "answers" } else { "not answering" },
        hc = match r.handshake_age { Some(a) if a < 180 => "ok", _ => "bad" },
        hs = match r.handshake_age {
            Some(a) if a < 180 => "up".to_string(),
            Some(a) => format!("last handshake {} ago", age(a)),
            None => "never handshaked".to_string(),
        },
        vc = if r.core_version.is_empty() || r.core_version == r.version { "" } else { "bad" },
        ver = if r.core_version.is_empty() || r.core_version == r.version {
            esc(&r.version)
        } else {
            format!("{} (the main router runs {}; update this access point)", esc(&r.version), esc(&r.core_version))
        },
        rc = if r.refused.is_some() { "bad" } else { "ok" },
        refused = match &r.refused { Some(e) => format!("refused: {}", esc(e)), None => "applied".to_string() },
    );
    let _ = write!(
        h,
        "<h2>Actions</h2>\
         <form method=\"post\" action=\"/restart\" onsubmit=\"return confirm('Restart this access point?')\">\
         <button>Restart</button></form>\
         <form method=\"post\" action=\"/factory-reset\" onsubmit=\"return confirm('Erase every setting on this access point and start over? It must be adopted again at the main router.')\">\
         <button>Factory reset</button></form>\
         <h2>Log</h2><p class=\"muted\">Link, tunnel, firewall and sync messages only. <a href=\"/logs\">Download</a></p>\
         <pre>{}</pre></body></html>",
        esc(log)
    );
    h
}

fn age(secs: u64) -> String {
    match secs {
        s if s < 120 => format!("{s} s"),
        s if s < 7200 => format!("{} min", s / 60),
        s => format!("{} h", s / 3600),
    }
}

fn html(body: String) -> Response<Body> {
    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .unwrap()
}

async fn page() -> Response<Body> {
    let role = satellite::load_role();
    let uplink = role.uplink.clone().unwrap_or_default();
    let name = role.name.clone().unwrap_or_else(|| "satellite".to_string());
    html(render(&name, &reasons(&uplink).await, &log_text().await))
}

async fn logs() -> Response<Body> {
    let text = log_text().await;
    Response::builder()
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"satellite-recovery.log\"",
        )
        .body(Body::from(text))
        .unwrap()
}

async fn restart() -> Response<Body> {
    crate::activity::log(
        "satellite",
        "recovery",
        true,
        "Restart from recovery mode",
        None,
    );
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let _ = crate::run_quiet_async(&mut tokio::process::Command::new("reboot")).await;
    });
    html("<!doctype html><meta charset=\"utf-8\"><p>Restarting. This page comes back if the access point is still dark.</p>".to_string())
}

async fn factory_reset() -> Response<Body> {
    crate::activity::log(
        "satellite",
        "recovery",
        true,
        "Factory reset from recovery mode",
        None,
    );
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let _ = crate::run_quiet_async(tokio::process::Command::new("firstboot").arg("-y")).await;
        let _ = crate::run_quiet_async(&mut tokio::process::Command::new("reboot")).await;
    });
    html("<!doctype html><meta charset=\"utf-8\"><p>Erasing and restarting. Adopt this router again at the main router.</p>".to_string())
}

async fn serve() -> Result<(), Error> {
    let app = Router::new()
        .route("/", get(page))
        .route("/logs", get(logs))
        .route("/restart", post(restart))
        .route("/factory-reset", post(factory_reset))
        .fallback(get(|| async {
            axum::response::Redirect::temporary("/").into_response()
        }));
    let addr = SocketAddr::from((ADDRESS, 80));
    // The address appears when netifd brings the interface up.
    let listener = loop {
        match startos::net::utils::bind_tokio_listener_reuse_port(addr) {
            Ok(l) => break l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrNotAvailable => {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(e) => return Err(Error::new(eyre!("bind {addr}: {e}"), ErrorKind::Network)),
        }
    };
    tracing::info!("recovery: page on http://{addr}/");
    axum::serve(listener, app)
        .await
        .with_kind(ErrorKind::Network)?;
    let _ = StatusCode::OK;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssid_takes_the_last_four_hex_digits_of_the_mac() {
        assert_eq!(ssid_for_mac("fe:fe:fe:82:5d:09"), "StartWRT-Recovery-5D09");
        assert_eq!(ssid_for_mac(""), "StartWRT-Recovery-0000");
    }

    #[test]
    fn log_filter_keeps_the_link_and_drops_clients() {
        let log = "Mon Oct 19 00:00:00 2026 daemon.notice netifd: Network device 'eth1' link is down\n\
                   Mon Oct 19 00:00:01 2026 daemon.info dnsmasq-dhcp[1]: DHCPACK(br-lan.101) 192.168.1.5 aa:bb\n\
                   Mon Oct 19 00:00:02 2026 daemon.notice hostapd: phy0-ap0: AP-STA-CONNECTED aa:bb\n\
                   Mon Oct 19 00:00:03 2026 user.warn startwrt-ctrld: liveness: the Core is unreachable\n";
        let f = filter_log(log);
        assert!(f.contains("link is down"));
        assert!(f.contains("Core is unreachable"));
        assert!(!f.contains("DHCPACK"));
        assert!(!f.contains("AP-STA"));
    }

    #[test]
    fn page_says_why_and_offers_the_two_actions() {
        let r = Reasons {
            uplink: "eth1".into(),
            uplink_link: false,
            core_neighbour: false,
            handshake_age: Some(600),
            version: "1.3.0".into(),
            core_version: "1.3.1".into(),
            refused: Some("generation 4: bad".into()),
        };
        let h = render("garage", &r, "x<y");
        assert!(h.contains("<h1>Satellite garage — recovery mode</h1>"));
        assert!(h.contains("check the cable"));
        assert!(h.contains("last handshake 10 min ago"));
        assert!(h.contains("update this access point"));
        assert!(h.contains("refused: generation 4: bad"));
        assert!(h.contains("action=\"/restart\"") && h.contains("action=\"/factory-reset\""));
        assert!(h.contains("x&lt;y"));
    }

    const NETWORK: &str = "\
config device\n\toption name 'br-lan'\n\toption type 'bridge'\n\tlist ports 'eth1'\n\tlist ports 'eth0'\n\n\
config interface 'lan'\n\toption device 'br-lan.1'\n\toption proto 'none'\n\n\
config interface 'yyiph'\n\toption device 'br-lan.101'\n\toption proto 'none'\n\n\
config interface 'backhaul'\n\toption device 'br-lan.4094'\n\toption proto 'none'\n\n\
config bridge-vlan\n\toption device 'br-lan'\n\toption vlan '1'\n\tlist ports 'eth1:t'\n\n\
config bridge-vlan\n\toption device 'br-lan'\n\toption vlan '101'\n\tlist ports 'eth1:t'\n\tlist ports 'eth0:u*'\n\n\
config bridge-vlan\n\toption device 'br-lan'\n\toption vlan '4094'\n\tlist ports 'eth1:u*'\n";
    const WIRELESS: &str = "\
config wifi-device 'radio0'\n\toption type 'mac80211'\n\toption band '2g'\n\toption channel '6'\n\toption htmode 'HE20'\n\n\
config wifi-iface 'default_radio0'\n\toption device 'radio0'\n\toption network 'lan'\n\toption mode 'ap'\n\toption ssid 'StartWRT'\n\toption encryption 'psk2'\n\toption key 'x'\n";
    const DHCP: &str = "config dnsmasq\n\toption domainneeded '1'\n\toption localservice '1'\n";

    #[tokio::test]
    async fn recovery_layout_is_isolated_and_reversible() {
        let dir = tempfile::tempdir().unwrap();
        for (f, c) in [
            ("network", NETWORK),
            ("wireless", WIRELESS),
            ("dhcp", DHCP),
            ("firewall", ""),
        ] {
            std::fs::write(dir.path().join(f), c).unwrap();
        }
        let arena = Arena::new();
        let mut cfgs = parse_all(dir.path(), &arena, CONFIGS).await.unwrap();
        write_layout(&mut cfgs, "eth1", Some("sticker")).unwrap();
        let dump = |cfgs: &Configs, n: &str| {
            cfgs[n]
                .sections
                .iter()
                .flat_map(|s| s.lines.iter().map(|l| l.to_string()))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let net = dump(&cfgs, "network");
        assert!(net.contains("option vlan '4093'"), "{net}");
        assert!(net.contains("option ipaddr '192.168.255.1'"), "{net}");
        // The access port left the guest VLAN for the recovery VLAN.
        let guest = cfgs["network"]
            .sections
            .iter()
            .filter_map(|s| s.get::<NetworkBridgeVlan>().ok())
            .find(|v| v.vlan == 101)
            .unwrap();
        assert!(guest.ports.iter().all(|p| p.port != "eth0"));
        let wl = dump(&cfgs, "wireless");
        assert!(wl.contains("StartWRT-Recovery-"), "{wl}");
        assert!(wl.contains("option maxassoc '1'"), "{wl}");
        assert!(wl.contains("option disabled '1'"), "{wl}");
        assert!(dump(&cfgs, "dhcp").contains("/router.lan/192.168.255.1"));
        let fw = dump(&cfgs, "firewall");
        assert!(
            fw.contains("name 'recovery'") && !fw.contains("config forwarding"),
            "{fw}"
        );

        remove_layout(&mut cfgs, "eth1").unwrap();
        let net = dump(&cfgs, "network");
        assert!(
            !net.contains("4093") && !net.contains("192.168.255.1"),
            "{net}"
        );
        assert!(!dump(&cfgs, "wireless").contains("Recovery"));
        assert!(!dump(&cfgs, "wireless").contains("option disabled '1'"));
        assert!(!dump(&cfgs, "dhcp").contains("router.lan"));
        assert!(!dump(&cfgs, "firewall").contains("name 'recovery'"));
    }
}

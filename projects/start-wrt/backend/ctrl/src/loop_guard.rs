//! Loop detection on the LAN bridge's wired ports (design §6 risk 12).
//!
//! Each round the router sends a probe out every wired port on every VLAN the
//! port carries, straight to the wire, and listens on all of them. Hearing its
//! own probe on any port means a second path. Spanning tree runs on the
//! untagged VLAN alone, so it misses the loop an access port makes when it is
//! cabled into a switch port of one profile on the trunk; a probe on each VLAN
//! does not.
//!
//! The loop is cut at an access port: the router disables it on the bridge,
//! keeps probing through it, and reopens it once its probes stop coming back.
//! A disabled port still sends and hears probes on the wire.
//! An uplink or a lone trunk port is never cut. The guard runs only on a
//! bridge with a trunk or an uplink, the ports that carry the backhaul VLAN.

use std::collections::{BTreeMap, BTreeSet};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::backhaul::BACKHAUL_VLAN;

/// IEEE 802 local experimental ethertype 1.
pub const ETHERTYPE: u16 = 0x88B5;
const VLAN_ETHERTYPE: u16 = 0x8100;
const MAGIC: &[u8; 4] = b"SWLP";
const VERSION: u8 = 1;
const ROUND: Duration = Duration::from_secs(1);
/// Rounds between rereads of the bridge's VLAN table.
const VLAN_REFRESH: u32 = 5;
/// Quiet rounds before a cut port reopens. Random within the range, so two
/// routers that cut the same loop do not reopen together.
const QUIET_MIN: u32 = 5;
const QUIET_MAX: u32 = 10;
const STATE_DISABLED: u8 = 0;
const STATE_FORWARDING: u8 = 3;

static BLOCKED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Ports this router has cut to break a loop.
pub fn blocked() -> Vec<String> {
    BLOCKED.lock().unwrap().iter().cloned().collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortVlans {
    pub port: String,
    /// VLAN id and whether the port sends it untagged.
    pub vlans: Vec<(u16, bool)>,
}

impl PortVlans {
    /// A main router's trunk port or an access point's uplink.
    pub fn is_trunk(&self) -> bool {
        self.vlans.iter().any(|(v, _)| *v == BACKHAUL_VLAN)
    }
}

/// `bridge -j vlan show`, wired ports only.
pub fn parse_vlans(json: &str, bridge: &str) -> Vec<PortVlans> {
    #[derive(serde::Deserialize)]
    struct Entry {
        ifname: String,
        #[serde(default)]
        vlans: Vec<Vlan>,
    }
    #[derive(serde::Deserialize)]
    struct Vlan {
        vlan: u16,
        #[serde(default)]
        flags: Vec<String>,
    }
    let Ok(entries) = serde_json::from_str::<Vec<Entry>>(json) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter(|e| e.ifname != bridge && wired(&e.ifname))
        .map(|e| PortVlans {
            port: e.ifname,
            vlans: e
                .vlans
                .into_iter()
                .map(|v| (v.vlan, v.flags.iter().any(|f| f == "Egress Untagged")))
                .collect(),
        })
        .collect()
}

fn wired(port: &str) -> bool {
    !port.starts_with("wlan") && !port.starts_with("phy")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub id: [u8; 8],
    pub port: String,
    pub vid: u16,
}

pub fn build_probe(src: [u8; 6], dst: [u8; 6], probe: &Probe, tagged: bool) -> Vec<u8> {
    let mut f = Vec::with_capacity(64);
    f.extend_from_slice(&dst);
    f.extend_from_slice(&src);
    if tagged {
        f.extend_from_slice(&VLAN_ETHERTYPE.to_be_bytes());
        f.extend_from_slice(&(probe.vid & 0x0fff).to_be_bytes());
    }
    f.extend_from_slice(&ETHERTYPE.to_be_bytes());
    f.extend_from_slice(MAGIC);
    f.push(VERSION);
    f.extend_from_slice(&probe.id);
    f.extend_from_slice(&probe.vid.to_be_bytes());
    let name = probe.port.as_bytes();
    let name = &name[..name.len().min(15)];
    f.push(name.len() as u8);
    f.extend_from_slice(name);
    let min = if tagged { 64 } else { 60 };
    if f.len() < min {
        f.resize(min, 0);
    }
    f
}

/// A probe from any router, tagged or not.
pub fn parse_probe(frame: &[u8]) -> Option<Probe> {
    let mut at = 12;
    let mut ty = u16::from_be_bytes(frame.get(at..at + 2)?.try_into().ok()?);
    if ty == VLAN_ETHERTYPE {
        at += 4;
        ty = u16::from_be_bytes(frame.get(at..at + 2)?.try_into().ok()?);
    }
    if ty != ETHERTYPE {
        return None;
    }
    let body = frame.get(at + 2..)?;
    if body.get(..4)? != MAGIC || *body.get(4)? != VERSION {
        return None;
    }
    let id: [u8; 8] = body.get(5..13)?.try_into().ok()?;
    let vid = u16::from_be_bytes(body.get(13..15)?.try_into().ok()?);
    let len = *body.get(15)? as usize;
    let port = std::str::from_utf8(body.get(16..16 + len)?)
        .ok()?
        .to_string();
    Some(Probe { id, port, vid })
}

/// A probe of this router's, sent out one port and heard on another, or on
/// the same one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Echo {
    pub sent: String,
    pub heard: String,
    pub vid: u16,
}

/// Where to cut the loop an echo shows: an access port it touched, the later
/// by name when both ends are; else the later of two trunk ports.
pub fn cut(echo: &Echo, ports: &[PortVlans]) -> Option<String> {
    let access = |p: &str| ports.iter().any(|v| v.port == p && !v.is_trunk());
    let trunk = |p: &str| ports.iter().any(|v| v.port == p && v.is_trunk());
    let later = || Some(echo.sent.clone().max(echo.heard.clone()));
    match (access(&echo.sent), access(&echo.heard)) {
        (true, true) => later(),
        (false, true) => Some(echo.heard.clone()),
        (true, false) => Some(echo.sent.clone()),
        (false, false) if trunk(&echo.sent) && trunk(&echo.heard) && echo.sent != echo.heard => {
            later()
        }
        (false, false) => None,
    }
}

/// Which ports are cut, and for how many more quiet rounds.
#[derive(Debug, Default)]
pub struct Guard {
    cut: BTreeMap<String, u32>,
}

impl Guard {
    /// Takes one round's cuts; returns the ports to block and to reopen.
    /// `quiet` gives the rounds a newly cut port waits.
    pub fn round(
        &mut self,
        cuts: &BTreeSet<String>,
        mut quiet: impl FnMut() -> u32,
    ) -> (Vec<String>, Vec<String>) {
        let mut block = Vec::new();
        for port in cuts {
            match self.cut.get_mut(port) {
                Some(left) => *left = quiet(),
                None => {
                    self.cut.insert(port.clone(), quiet());
                    block.push(port.clone());
                }
            }
        }
        let mut reopen = Vec::new();
        self.cut.retain(|port, left| {
            if cuts.contains(port) {
                return true;
            }
            *left = left.saturating_sub(1);
            if *left == 0 {
                reopen.push(port.clone());
                false
            } else {
                true
            }
        });
        (block, reopen)
    }

    /// Adopts a port found blocked at start, which only this guard does.
    pub fn adopt(&mut self, port: &str, quiet: u32) {
        self.cut.insert(port.to_string(), quiet);
    }

    pub fn ports(&self) -> impl Iterator<Item = &String> {
        self.cut.keys()
    }
}

// ── I/O ─────────────────────────────────────────────────────────────────────

const PACKET_IGNORE_OUTGOING: libc::c_int = 23;

struct Socket(OwnedFd);

impl Socket {
    fn open() -> std::io::Result<Self> {
        // SAFETY: plain syscalls on a descriptor this function owns.
        unsafe {
            let fd = libc::socket(
                libc::AF_PACKET,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                (libc::ETH_P_ALL as u16).to_be() as libc::c_int,
            );
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let sock = Socket(OwnedFd::from_raw_fd(fd));
            // ldh [12]; probe → accept; 802.1Q → ldh [16]; probe → accept; drop.
            let mut code = [
                libc::sock_filter {
                    code: 0x28,
                    jt: 0,
                    jf: 0,
                    k: 12,
                },
                libc::sock_filter {
                    code: 0x15,
                    jt: 3,
                    jf: 0,
                    k: ETHERTYPE as u32,
                },
                libc::sock_filter {
                    code: 0x15,
                    jt: 0,
                    jf: 3,
                    k: VLAN_ETHERTYPE as u32,
                },
                libc::sock_filter {
                    code: 0x28,
                    jt: 0,
                    jf: 0,
                    k: 16,
                },
                libc::sock_filter {
                    code: 0x15,
                    jt: 0,
                    jf: 1,
                    k: ETHERTYPE as u32,
                },
                libc::sock_filter {
                    code: 0x06,
                    jt: 0,
                    jf: 0,
                    k: 0xffff,
                },
                libc::sock_filter {
                    code: 0x06,
                    jt: 0,
                    jf: 0,
                    k: 0,
                },
            ];
            let prog = libc::sock_fprog {
                len: code.len() as u16,
                filter: code.as_mut_ptr(),
            };
            sock.setopt(libc::SOL_SOCKET, libc::SO_ATTACH_FILTER, &prog)?;
            sock.setopt(libc::SOL_PACKET, PACKET_IGNORE_OUTGOING, &1 as &libc::c_int)?;
            let timeout = libc::timeval {
                tv_sec: 0,
                tv_usec: 100_000,
            };
            sock.setopt(libc::SOL_SOCKET, libc::SO_RCVTIMEO, &timeout)?;
            Ok(sock)
        }
    }

    unsafe fn setopt<T>(
        &self,
        level: libc::c_int,
        name: libc::c_int,
        v: &T,
    ) -> std::io::Result<()> {
        let r = libc::setsockopt(
            self.0.as_raw_fd(),
            level,
            name,
            v as *const T as *const libc::c_void,
            std::mem::size_of::<T>() as libc::socklen_t,
        );
        if r < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn send(&self, ifindex: i32, frame: &[u8]) -> std::io::Result<()> {
        // SAFETY: the address and buffer outlive the call.
        unsafe {
            let mut addr: libc::sockaddr_ll = std::mem::zeroed();
            addr.sll_family = libc::AF_PACKET as u16;
            addr.sll_ifindex = ifindex;
            addr.sll_halen = 6;
            addr.sll_addr[..6].copy_from_slice(&frame[..6]);
            let r = libc::sendto(
                self.0.as_raw_fd(),
                frame.as_ptr() as *const libc::c_void,
                frame.len(),
                0,
                &addr as *const libc::sockaddr_ll as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
            );
            if r < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// One frame and the index of the port it arrived on; `None` on timeout.
    fn recv(&self, buf: &mut [u8]) -> Option<(usize, i32)> {
        // SAFETY: the address and buffer outlive the call.
        unsafe {
            let mut addr: libc::sockaddr_ll = std::mem::zeroed();
            let mut len = std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t;
            let n = libc::recvfrom(
                self.0.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
                &mut addr as *mut libc::sockaddr_ll as *mut libc::sockaddr,
                &mut len,
            );
            if n <= 0 || addr.sll_pkttype == libc::PACKET_OUTGOING as u8 {
                return None;
            }
            Some((n as usize, addr.sll_ifindex))
        }
    }
}

fn sysfs(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn ifindex(port: &str) -> Option<i32> {
    sysfs(&format!("/sys/class/net/{port}/ifindex"))?
        .parse()
        .ok()
}

fn port_state(port: &str) -> Option<u8> {
    sysfs(&format!("/sys/class/net/{port}/brport/state"))?
        .parse()
        .ok()
}

fn link_up(port: &str) -> bool {
    sysfs(&format!("/sys/class/net/{port}/operstate")).as_deref() == Some("up")
}

fn bridge_stp(bridge: &str) -> bool {
    sysfs(&format!("/sys/class/net/{bridge}/bridge/stp_state")).is_some_and(|s| s != "0")
}

fn set_state(port: &str, state: u8) -> bool {
    std::process::Command::new("bridge")
        .args(["link", "set", "dev", port, "state", &state.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn read_vlans(bridge: &str) -> Vec<PortVlans> {
    std::process::Command::new("bridge")
        .args(["-j", "vlan", "show"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_vlans(&String::from_utf8_lossy(&o.stdout), bridge))
        .unwrap_or_default()
}

/// Locally administered unicast: the source no host owns, and a destination
/// every bridge floods and no radio sends.
fn random_mac() -> [u8; 6] {
    let mut mac: [u8; 6] = rand::random();
    mac[0] = (mac[0] & 0xfc) | 0x02;
    mac
}

fn quiet() -> u32 {
    rand::random_range(QUIET_MIN..=QUIET_MAX)
}

/// Starts the guard on its own thread, once.
pub fn start() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        if let Err(e) = std::thread::Builder::new()
            .name("loop-guard".into())
            .spawn(run)
        {
            tracing::error!("loop guard: failed to start thread: {e}");
        }
    });
}

fn run() {
    let bridge = crate::ethernet::DEFAULT_LAN_BRIDGE;
    let sock = match Socket::open() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("loop guard: no packet socket: {e}");
            return;
        }
    };
    let id: [u8; 8] = rand::random();
    let src = random_mac();
    let dst = random_mac();
    let mut guard = Guard::default();
    let mut ports = Vec::new();
    let mut round = 0u32;
    let mut buf = [0u8; 256];
    let mut adopted = false;
    let mut beyond = BTreeSet::new();
    loop {
        let deadline = Instant::now() + ROUND;
        if round % VLAN_REFRESH == 0 {
            ports = read_vlans(bridge);
        }
        round = round.wrapping_add(1);
        let active = ports.iter().any(PortVlans::is_trunk) && !bridge_stp(bridge);
        if !active {
            for port in guard.ports().cloned().collect::<Vec<_>>() {
                reopen(&port);
            }
            guard = Guard::default();
            publish(&guard);
            std::thread::sleep(ROUND);
            continue;
        }
        if !adopted {
            // A port with a link stays disabled only by this guard's hand.
            for p in &ports {
                if link_up(&p.port) && port_state(&p.port) == Some(STATE_DISABLED) {
                    guard.adopt(&p.port, quiet());
                }
            }
            publish(&guard);
            adopted = true;
        }
        let index: BTreeMap<i32, String> = ports
            .iter()
            .filter_map(|p| Some((ifindex(&p.port)?, p.port.clone())))
            .collect();
        for p in ports.iter().filter(|p| link_up(&p.port)) {
            let Some(ix) = ifindex(&p.port) else { continue };
            for &(vid, untagged) in &p.vlans {
                let probe = Probe {
                    id,
                    port: p.port.clone(),
                    vid,
                };
                if let Err(e) = sock.send(ix, &build_probe(src, dst, &probe, !untagged)) {
                    tracing::debug!("loop guard: probe on {} VLAN {vid}: {e}", p.port);
                }
            }
        }
        let mut echoes = BTreeSet::new();
        while Instant::now() < deadline {
            let Some((n, ix)) = sock.recv(&mut buf) else {
                continue;
            };
            let (Some(heard), Some(probe)) = (index.get(&ix), parse_probe(&buf[..n])) else {
                continue;
            };
            if probe.id == id {
                echoes.insert(Echo {
                    sent: probe.port,
                    heard: heard.clone(),
                    vid: probe.vid,
                });
            }
        }
        let mut cuts = BTreeSet::new();
        let mut uncut = BTreeSet::new();
        for echo in &echoes {
            match cut(echo, &ports) {
                Some(port) => {
                    cuts.insert(port);
                }
                None => {
                    if !beyond.contains(echo) {
                        tracing::warn!(
                            "loop guard: a loop beyond {} (VLAN {}); not cut here",
                            echo.sent,
                            echo.vid
                        );
                    }
                    uncut.insert(echo.clone());
                }
            }
        }
        beyond = uncut;
        let (block, reopened) = guard.round(&cuts, quiet);
        for port in &block {
            let echo = echoes
                .iter()
                .find(|e| cut(e, &ports).as_ref() == Some(port))
                .expect("a cut comes from an echo");
            set_state(port, STATE_DISABLED);
            let detail = format!(
                "A probe sent out {} on VLAN {} came back on {}",
                echo.sent, echo.vid, echo.heard
            );
            tracing::warn!("loop guard: {port} cut: {detail}");
            crate::activity::log(
                "ethernet",
                "loop",
                false,
                &format!("Loop detected: port {port} stopped forwarding"),
                Some(&detail),
            );
        }
        for port in &reopened {
            reopen(port);
            crate::activity::log(
                "ethernet",
                "loop",
                true,
                &format!("Port {port} forwarding again: the loop is gone"),
                None,
            );
        }
        // A network restart or a link coming up sets a port forwarding.
        for port in guard.ports() {
            if port_state(port).is_some_and(|s| s != STATE_DISABLED) {
                set_state(port, STATE_DISABLED);
            }
        }
        if !block.is_empty() || !reopened.is_empty() {
            publish(&guard);
        }
    }
}

fn reopen(port: &str) {
    tracing::info!("loop guard: {port} reopened");
    set_state(port, STATE_FORWARDING);
}

fn publish(guard: &Guard) {
    *BLOCKED.lock().unwrap() = guard.ports().cloned().collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    const AP_VLANS: &str = r#"[{"ifname":"eth0","vlans":[{"vlan":101,"flags":["PVID","Egress Untagged"]}]},{"ifname":"eth1","vlans":[{"vlan":1},{"vlan":101},{"vlan":4094,"flags":["PVID","Egress Untagged"]}]},{"ifname":"br-lan","vlans":[{"vlan":1},{"vlan":101},{"vlan":4094}]},{"ifname":"phy0-ap0","vlans":[{"vlan":1,"flags":["PVID","Egress Untagged"]}]}]"#;

    fn ap() -> Vec<PortVlans> {
        parse_vlans(AP_VLANS, "br-lan")
    }

    fn echo(sent: &str, heard: &str) -> Echo {
        Echo {
            sent: sent.into(),
            heard: heard.into(),
            vid: 101,
        }
    }

    #[test]
    fn vlans_keep_wired_ports_and_their_tagging() {
        let ports = ap();
        assert_eq!(
            ports,
            vec![
                PortVlans {
                    port: "eth0".into(),
                    vlans: vec![(101, true)]
                },
                PortVlans {
                    port: "eth1".into(),
                    vlans: vec![(1, false), (101, false), (4094, true)]
                },
            ]
        );
        assert!(!ports[0].is_trunk());
        assert!(ports[1].is_trunk());
        assert!(parse_vlans("not json", "br-lan").is_empty());
    }

    #[test]
    fn a_probe_survives_the_wire_tagged_or_not() {
        let probe = Probe {
            id: *b"abcdefgh",
            port: "eth1".into(),
            vid: 101,
        };
        let src = [2, 0, 0, 0, 0, 1];
        let dst = [2, 0, 0, 0, 0, 2];
        let untagged = build_probe(src, dst, &probe, false);
        assert_eq!(untagged.len(), 60);
        assert_eq!(&untagged[12..14], &[0x88, 0xb5]);
        assert_eq!(parse_probe(&untagged), Some(probe.clone()));
        let tagged = build_probe(src, dst, &probe, true);
        assert_eq!(&tagged[12..16], &[0x81, 0x00, 0x00, 101]);
        assert_eq!(parse_probe(&tagged), Some(probe));
        let mut other = untagged.clone();
        other[16] = b'X';
        assert_eq!(parse_probe(&other), None);
        assert_eq!(parse_probe(&untagged[..20]), None);
    }

    #[test]
    fn a_loop_is_cut_at_an_access_port_and_never_at_a_lone_uplink() {
        let ports = ap();
        // An access port cabled into a profile port of a switch on the trunk:
        // each direction names the access port.
        assert_eq!(cut(&echo("eth0", "eth1"), &ports), Some("eth0".into()));
        assert_eq!(cut(&echo("eth1", "eth0"), &ports), Some("eth0".into()));
        // A loop beyond the uplink alone is not this router's to cut.
        assert_eq!(cut(&echo("eth1", "eth1"), &ports), None);
        // A radio is not cut; the wired port the probe left by is.
        assert_eq!(cut(&echo("eth0", "phy0-ap0"), &ports), Some("eth0".into()));
        assert_eq!(cut(&echo("eth1", "phy0-ap0"), &ports), None);
    }

    #[test]
    fn two_access_ports_on_one_loop_cut_the_same_one_from_either_side() {
        let ports = vec![
            PortVlans {
                port: "lan1".into(),
                vlans: vec![(101, true)],
            },
            PortVlans {
                port: "lan2".into(),
                vlans: vec![(102, true)],
            },
        ];
        assert_eq!(cut(&echo("lan1", "lan2"), &ports), Some("lan2".into()));
        assert_eq!(cut(&echo("lan2", "lan1"), &ports), Some("lan2".into()));
        // A loop behind one access port, such as a switch cabled to itself.
        assert_eq!(cut(&echo("lan1", "lan1"), &ports), Some("lan1".into()));
    }

    #[test]
    fn two_trunk_ports_on_one_loop_cut_the_same_one_from_either_side() {
        let ports = vec![
            PortVlans {
                port: "lan1".into(),
                vlans: vec![(4094, true)],
            },
            PortVlans {
                port: "lan2".into(),
                vlans: vec![(4094, true)],
            },
        ];
        assert_eq!(cut(&echo("lan1", "lan2"), &ports), Some("lan2".into()));
        assert_eq!(cut(&echo("lan2", "lan1"), &ports), Some("lan2".into()));
    }

    #[test]
    fn a_cut_port_reopens_after_its_quiet_rounds_and_a_new_echo_restarts_them() {
        let mut g = Guard::default();
        let eth0: BTreeSet<String> = ["eth0".to_string()].into();
        let none = BTreeSet::new();
        assert_eq!(g.round(&eth0, || 3), (vec!["eth0".to_string()], vec![]));
        assert_eq!(g.round(&eth0, || 3), (vec![], vec![]));
        assert_eq!(g.round(&none, || 3), (vec![], vec![]));
        assert_eq!(g.round(&none, || 3), (vec![], vec![]));
        // Heard again: the count starts over.
        assert_eq!(g.round(&eth0, || 3), (vec![], vec![]));
        assert_eq!(g.round(&none, || 3), (vec![], vec![]));
        assert_eq!(g.round(&none, || 3), (vec![], vec![]));
        assert_eq!(g.round(&none, || 3), (vec![], vec!["eth0".to_string()]));
        assert_eq!(g.ports().count(), 0);
    }

    #[test]
    fn a_port_found_blocked_at_start_is_adopted_and_reopens() {
        let mut g = Guard::default();
        g.adopt("eth0", 1);
        assert_eq!(
            g.round(&BTreeSet::new(), || 1),
            (vec![], vec!["eth0".to_string()])
        );
    }
}

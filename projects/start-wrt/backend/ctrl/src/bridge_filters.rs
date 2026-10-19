//! Bridge-family nftables files the daemon writes into fw4's include
//! directory (design D18, D25): the Core's trunk gate and the access point's
//! variant of the DHCP guard. Nothing goes under `/etc/nftables.d`, which
//! sysupgrade restores over the image; `/usr/share` is the image's, so a
//! file here is rewritten on first boot after an upgrade.
//!
//! Both files match the ingress or egress port, which needs
//! `kmod-nft-bridge`. Without it the gate is not written and the trunk stays
//! open, and an access point runs no guard at all rather than the Core's,
//! which would drop the Core's DHCP replies to its clients.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

use crate::prelude::*;

pub const GUARD_PATH: &str = "/usr/share/nftables.d/ruleset-pre/10-startwrt-lan-dhcp-guard.nft";
pub const GATE_PATH: &str = "/usr/share/nftables.d/ruleset-pre/20-startwrt-trunk-gate.nft";
const SHIPPED_GUARD: &str =
    include_str!("../../nftables/ruleset-pre/10-startwrt-lan-dhcp-guard.nft");
pub const GATE_TABLE: &str = "startwrt_trunk_gate";
pub const LIVE_SET: &str = "live";
const BRIDGE_META_MODULE: &str = "nft_meta_bridge";

const GATE: &str = "\
# Profile VLANs cross a trunk port only while an adopted access point's
# management tunnel is up through it (startwrt satellite). The daemon fills
# the set; empty is closed. The backhaul VLAN is never gated.
table bridge startwrt_trunk_gate
delete table bridge startwrt_trunk_gate
table bridge startwrt_trunk_gate {
	set live {
		type ifname
	}
	chain ingress {
		type filter hook prerouting priority -300; policy accept;
		vlan id != 4094 iifname != @live counter drop
	}
	chain egress {
		type filter hook postrouting priority 300; policy accept;
		vlan id != 4094 oifname != @live counter drop
	}
}
";

/// The shipped guard with the uplink exempted: the Core's replies and
/// advertisements come in there, a rogue server's from any other port.
pub fn ap_guard(uplink: &str) -> Result<String, Error> {
    if uplink.is_empty()
        || !uplink
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
    {
        return Err(Error::new(
            eyre!("not an interface name: {uplink:?}"),
            ErrorKind::InvalidValue,
        ));
    }
    let hook = "type filter hook forward priority filter; policy accept;\n";
    let Some(at) = SHIPPED_GUARD.find(hook) else {
        return Err(Error::new(
            eyre!("the shipped DHCP guard has no forward hook"),
            ErrorKind::InvalidValue,
        ));
    };
    let split = at + hook.len();
    Ok(format!(
        "{}\t\tiifname \"{uplink}\" accept\n{}",
        &SHIPPED_GUARD[..split],
        &SHIPPED_GUARD[split..]
    ))
}

/// Whether the kernel can match a bridge port by name.
pub fn meta_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        Path::new("/sys/module").join(BRIDGE_META_MODULE).exists()
            || std::process::Command::new("modprobe")
                .arg(BRIDGE_META_MODULE)
                .output()
                .is_ok_and(|o| o.status.success())
    })
}

async fn write_if_changed(path: &str, content: &str) -> Result<bool, Error> {
    if tokio::fs::read_to_string(path)
        .await
        .is_ok_and(|c| c == content)
    {
        return Ok(false);
    }
    if let Some(dir) = Path::new(path).parent() {
        tokio::fs::create_dir_all(dir).await.map_err(|e| {
            Error::new(eyre!("mkdir {}: {e}", dir.display()), ErrorKind::Filesystem)
        })?;
    }
    tokio::fs::write(path, content)
        .await
        .map_err(|e| Error::new(eyre!("write {path}: {e}"), ErrorKind::Filesystem))?;
    Ok(true)
}

async fn remove_if_present(path: &str) -> Result<bool, Error> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::new(
            eyre!("remove {path}: {e}"),
            ErrorKind::Filesystem,
        )),
    }
}

/// A Core's files: the gate while it has a trunk, the guard as shipped.
/// Returns whether anything changed, which calls for a firewall reload.
pub async fn sync_core(has_trunk: bool) -> Result<bool, Error> {
    let mut changed = false;
    if Path::new(GUARD_PATH).exists() {
        changed |= write_if_changed(GUARD_PATH, SHIPPED_GUARD).await?;
    }
    if has_trunk && meta_available() {
        changed |= write_if_changed(GATE_PATH, GATE).await?;
    } else {
        if has_trunk {
            tracing::warn!(
                "trunk gating unavailable: {BRIDGE_META_MODULE} is not in this image; the trunk stays open"
            );
        }
        changed |= remove_if_present(GATE_PATH).await?;
    }
    Ok(changed)
}

/// An access point's files: no gate, and the guard variant or no guard.
pub async fn sync_ap(uplink: &str) -> Result<bool, Error> {
    let mut changed = remove_if_present(GATE_PATH).await?;
    if meta_available() {
        changed |= write_if_changed(GUARD_PATH, &ap_guard(uplink)?).await?;
    } else {
        changed |= remove_if_present(GUARD_PATH).await?;
    }
    Ok(changed)
}

pub fn gate_installed() -> bool {
    Path::new(GATE_PATH).exists()
}

/// One transaction: the set is never empty between the flush and the add.
pub fn live_set_script(ports: &BTreeSet<String>) -> String {
    let mut script = format!("flush set bridge {GATE_TABLE} {LIVE_SET}\n");
    if !ports.is_empty() {
        let elements = ports.iter().cloned().collect::<Vec<_>>().join(", ");
        script.push_str(&format!(
            "add element bridge {GATE_TABLE} {LIVE_SET} {{ {elements} }}\n"
        ));
    }
    script
}

/// Replaces the live set with `ports`.
pub async fn set_live_ports(ports: &BTreeSet<String>) -> Result<(), Error> {
    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new("nft")
        .args(["-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::new(eyre!("nft: {e}"), ErrorKind::Network))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(live_set_script(ports).as_bytes()).await;
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| Error::new(eyre!("nft: {e}"), ErrorKind::Network))?;
    if !out.status.success() {
        return Err(Error::new(
            eyre!(
                "nft refused the live set: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            ErrorKind::Network,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_closes_every_profile_vlan_on_an_unknown_port_and_spares_the_backhaul() {
        assert!(GATE.contains("set live"));
        assert!(GATE.contains("vlan id != 4094 iifname != @live counter drop"));
        assert!(GATE.contains("vlan id != 4094 oifname != @live counter drop"));
        assert!(GATE.starts_with('#'));
    }

    #[test]
    fn ap_guard_exempts_the_uplink_ahead_of_the_drops() {
        let g = ap_guard("eth1").unwrap();
        let accept = g.find("iifname \"eth1\" accept").unwrap();
        let drop = g.find("counter drop").unwrap();
        assert!(accept < drop, "{g}");
        assert_eq!(g.matches("counter drop").count(), 3);
        assert!(ap_guard("").is_err());
        assert!(ap_guard("eth1\"; flush ruleset").is_err());
    }

    #[test]
    fn live_set_script_flushes_then_adds_in_one_transaction() {
        let empty = live_set_script(&BTreeSet::new());
        assert_eq!(empty, "flush set bridge startwrt_trunk_gate live\n");
        let two = live_set_script(&["eth2".to_string(), "eth0".to_string()].into());
        assert_eq!(
            two,
            "flush set bridge startwrt_trunk_gate live\n\
             add element bridge startwrt_trunk_gate live { eth0, eth2 }\n"
        );
    }
}

//! What an access point may reach through its Core (design D17, D25): the
//! update source the Core is itself configured with, and the hosts that
//! source's asset URLs name. The Core resolves them, keeps two fw4 sets
//! current, and forwards nothing else out of the tunnel zone.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::time::Duration;

use uciedit::openwrt::{FirewallIpset, FirewallRule, FirewallTarget};
use uciedit::Configs;

use crate::mgmt_tunnel::CORE_ZONE;
use crate::prelude::*;

pub const REFRESH: Duration = Duration::from_secs(10 * 60);
const WAN_ZONE: &str = "wan";
const SETS: [(&str, &str, &str, &str, &str); 2] = [
    (
        "sat_reach4",
        "ipv4",
        "/etc/startwrt/sat-reach4",
        "allow_satellite_update4",
        "Allow-Satellite-Update-IPv4",
    ),
    (
        "sat_reach6",
        "ipv6",
        "/etc/startwrt/sat-reach6",
        "allow_satellite_update6",
        "Allow-Satellite-Update-IPv6",
    ),
];

/// The sets and the rules that pass HTTPS from the tunnel zone to them. The
/// load files exist before the firewall first reads them.
pub fn ensure_rules(cfgs: &mut Configs) -> Result<(), Error> {
    for (set, family, file, section, rule) in SETS {
        if !std::path::Path::new(file).exists() {
            let _ = std::fs::create_dir_all("/etc/startwrt");
            let _ = std::fs::write(file, "");
        }
        if !cfgs["firewall"]
            .sections
            .iter()
            .any(|s| s.get::<FirewallIpset>().is_ok_and(|i| i.name == set))
        {
            cfgs["firewall"].append(
                &FirewallIpset {
                    name: set.to_string(),
                    family: Some(family.to_string()),
                    matches: vec!["dest_ip".to_string()],
                    loadfile: Some(file.to_string()),
                    enabled: None,
                },
                Some(set),
            )?;
        }
        if !cfgs["firewall"]
            .sections
            .iter()
            .any(|s| s.get::<FirewallRule>().is_ok_and(|r| r.name == rule))
        {
            cfgs["firewall"].append(
                &FirewallRule {
                    name: rule.to_string(),
                    src: CORE_ZONE.to_string(),
                    dest: Some(WAN_ZONE.to_string()),
                    dest_port: Some("443".to_string()),
                    proto: vec!["tcp".to_string()],
                    target: FirewallTarget::ACCEPT,
                    family: Some(family.to_string()),
                    ipset: Some(set.to_string()),
                    ..Default::default()
                },
                Some(section),
            )?;
        }
    }
    Ok(())
}

pub fn remove_rules(cfgs: &mut Configs) {
    let sets: Vec<&str> = SETS.iter().map(|s| s.0).collect();
    let rules: Vec<&str> = SETS.iter().map(|s| s.4).collect();
    cfgs["firewall"].sections.retain(|s| {
        !s.get::<FirewallIpset>()
            .is_ok_and(|i| sets.contains(&i.name.as_str()))
            && !s
                .get::<FirewallRule>()
                .is_ok_and(|r| rules.contains(&r.name.as_str()))
    });
}

/// The registry host and every host its asset URLs name.
pub fn hosts(
    registry_url: &str,
    versions: &[crate::registry::os::OsVersionInfo],
) -> BTreeSet<String> {
    let mut hosts = BTreeSet::new();
    if let Some(h) = url::Url::parse(registry_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
    {
        hosts.insert(h);
    }
    for v in versions {
        for asset in v
            .squashfs
            .values()
            .chain(v.img.values())
            .chain(v.iso.values())
        {
            for u in &asset.urls {
                if let Some(h) = u.host_str() {
                    hosts.insert(h.to_string());
                }
            }
        }
    }
    hosts
}

fn script(set: &str, addrs: &[IpAddr]) -> String {
    let mut s = format!("flush set inet fw4 {set}\n");
    if !addrs.is_empty() {
        let elements = addrs
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        s.push_str(&format!("add element inet fw4 {set} {{ {elements} }}\n"));
    }
    s
}

async fn apply(set: &str, file: &str, addrs: &[IpAddr]) -> Result<(), Error> {
    use tokio::io::AsyncWriteExt;

    let content = addrs.iter().map(|a| format!("{a}\n")).collect::<String>();
    if !tokio::fs::read_to_string(file)
        .await
        .is_ok_and(|c| c == content)
    {
        tokio::fs::write(file, &content)
            .await
            .map_err(|e| Error::new(eyre!("write {file}: {e}"), ErrorKind::Filesystem))?;
    }
    let mut child = tokio::process::Command::new("nft")
        .args(["-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::new(eyre!("nft: {e}"), ErrorKind::Network))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(script(set, addrs).as_bytes()).await;
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| Error::new(eyre!("nft: {e}"), ErrorKind::Network))?;
    if !out.status.success() {
        return Err(Error::new(
            eyre!(
                "nft refused {set}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            ErrorKind::Network,
        ));
    }
    Ok(())
}

/// Resolves the update source and its asset hosts and installs the
/// addresses. Returns how many hosts resolved.
pub async fn refresh() -> Result<usize, Error> {
    let registry_url = crate::system::read_registry_url()
        .unwrap_or_else(|| crate::system::DEFAULT_REGISTRY_URL.to_string());
    let versions: Vec<_> = match crate::update::fetch_newer_versions(&registry_url).await {
        Ok(v) => v.into_values().collect(),
        Err(e) => {
            tracing::debug!("update reach: version index not fetched: {e}");
            Vec::new()
        }
    };
    let mut v4 = BTreeSet::new();
    let mut v6 = BTreeSet::new();
    let mut resolved = 0;
    for host in hosts(&registry_url, &versions) {
        match tokio::net::lookup_host((host.as_str(), 443)).await {
            Ok(addrs) => {
                resolved += 1;
                for a in addrs {
                    match a.ip() {
                        IpAddr::V4(ip) => v4.insert(IpAddr::V4(ip)),
                        IpAddr::V6(ip) => v6.insert(IpAddr::V6(ip)),
                    };
                }
            }
            Err(e) => tracing::debug!("update reach: {host} did not resolve: {e}"),
        }
    }
    let v4: Vec<IpAddr> = v4.into_iter().collect();
    let v6: Vec<IpAddr> = v6.into_iter().collect();
    apply(SETS[0].0, SETS[0].2, &v4).await?;
    apply(SETS[1].0, SETS[1].2, &v6).await?;
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use uciedit::{parse_all, Arena};

    use super::*;

    #[tokio::test]
    async fn rules_pass_https_from_the_tunnel_zone_through_the_sets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("firewall"), "").unwrap();
        let arena = Arena::new();
        let mut cfgs = parse_all(dir.path(), &arena, &["firewall"]).await.unwrap();
        ensure_rules(&mut cfgs).unwrap();
        ensure_rules(&mut cfgs).unwrap();
        let sets: Vec<FirewallIpset> = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<FirewallIpset>().ok())
            .collect();
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0].matches, vec!["dest_ip"]);
        let rules: Vec<FirewallRule> = cfgs["firewall"]
            .sections
            .iter()
            .filter_map(|s| s.get::<FirewallRule>().ok())
            .collect();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].src, CORE_ZONE);
        assert_eq!(rules[0].dest.as_deref(), Some("wan"));
        assert_eq!(rules[0].ipset.as_deref(), Some("sat_reach4"));
        assert_eq!(rules[1].family.as_deref(), Some("ipv6"));
        remove_rules(&mut cfgs);
        assert!(cfgs["firewall"].sections.is_empty());
    }

    #[test]
    fn hosts_cover_the_registry_and_its_assets() {
        let mut info = crate::registry::os::OsVersionInfo::default();
        let asset: crate::registry::asset::RegistryAsset<
            crate::sign::commitment::Blake3Commitment,
        > = serde_json::from_value(serde_json::json!({
            "publishedAt": "2026-01-01T00:00:00Z",
            "urls": ["https://cdn.example.net/x.bin", "https://mirror.example.org/x.bin"],
            "commitment": { "hash": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", "size": 1 },
            "signatures": {}
        }))
        .unwrap();
        info.squashfs.insert("k1".into(), asset);
        let h = hosts("https://registry.example.com/", &[info]);
        assert_eq!(
            h.into_iter().collect::<Vec<_>>(),
            vec![
                "cdn.example.net",
                "mirror.example.org",
                "registry.example.com"
            ]
        );
    }

    #[test]
    fn script_flushes_then_adds() {
        assert_eq!(script("sat_reach4", &[]), "flush set inet fw4 sat_reach4\n");
        let s = script("sat_reach4", &["1.2.3.4".parse().unwrap()]);
        assert!(s.ends_with("add element inet fw4 sat_reach4 { 1.2.3.4 }\n"));
    }
}

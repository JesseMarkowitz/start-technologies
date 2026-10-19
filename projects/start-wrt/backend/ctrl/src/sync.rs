//! Config sync (design D5, D20): the Core builds each access point's payload
//! from its own Wi-Fi, profiles and port assignments, numbers it, and pushes
//! it over the tunnel whenever it changes or an access point falls behind.
//!
//! The payload is derived, never edited: every tick the Core rebuilds it from
//! its configs and compares a digest with the last one, so a password, profile
//! or port change reaches every access point within seconds with no hook in
//! the code that made the change. The generation is a counter the Core keeps;
//! an access point refuses anything older than what it applied.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ap::ADMIN_VLAN;
use crate::prelude::*;
use crate::profiles::ProfileId;
use crate::satellite::{
    ApUpdateState, PairedSatellite, PasswordSpec, ProfileSpec, RadioInfo, RadioPlan, SyncSnapshot,
    SYNC_FORMAT,
};
use crate::wifi::Wifi;
use crate::ServerContext;

const STATE_PATH: &str = "/etc/startwrt/sync.json";
/// A push that failed is not retried before this; one that succeeded holds
/// nothing back.
const RETRY_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    generation: u64,
    digest: String,
}

fn load_state() -> State {
    std::fs::read_to_string(STATE_PATH)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

async fn save_state(state: &State) -> Result<(), Error> {
    let content = serde_json::to_string(state)
        .map_err(|e| Error::new(eyre!("serialize sync state: {e}"), ErrorKind::Serialization))?;
    tokio::fs::write(STATE_PATH, content)
        .await
        .map_err(|e| Error::new(eyre!("write {STATE_PATH}: {e}"), ErrorKind::Filesystem))
}

/// The Core's side of every payload.
pub struct Inputs {
    pub wifi: Wifi<ProfileId>,
    pub profiles: Vec<ProfileId>,
    pub update_source: String,
}

async fn gather() -> Result<Inputs, Error> {
    let ctx = ServerContext::default();
    let wifi = crate::wifi::get(ctx.clone()).await?;
    let profiles = {
        let arena = uciedit::Arena::new();
        let cfgs = uciedit::parse_all("/etc/config", &arena, &["startwrt"]).await?;
        crate::profiles::list_config(ctx, &cfgs)?
    };
    Ok(Inputs {
        wifi,
        profiles,
        update_source: crate::system::read_registry_url()
            .unwrap_or_else(|| crate::system::DEFAULT_REGISTRY_URL.to_string()),
    })
}

// ── Channel plan (D20) ──────────────────────────────────────────────────────

const CHANNELS_2G: [u32; 3] = [1, 6, 11];
const CHANNELS_5G: [u32; 4] = [36, 40, 44, 48];

fn band_channel(band: &str) -> Option<(&'static [u32], &'static str)> {
    match band {
        "2g" => Some((&CHANNELS_2G, "HE20")),
        "5g" => Some((&CHANNELS_5G, "HE80")),
        _ => None,
    }
}

/// Picks a band and the least-used channel for each of an access point's
/// radios. `taken` holds every (band, channel) already planned or in use on
/// the Core and other access points. The first radio of a dual-band phy takes
/// 2.4 GHz for reach; a second radio takes the other band.
pub fn plan(radios: &[RadioInfo], taken: &[(String, u32)]) -> BTreeMap<String, RadioPlan> {
    let mut out = BTreeMap::new();
    let mut used_bands: BTreeSet<&str> = BTreeSet::new();
    let mut taken: Vec<(String, u32)> = taken.to_vec();
    for radio in radios {
        let band = ["2g", "5g", "6g"]
            .into_iter()
            .find(|b| radio.bands.iter().any(|rb| rb == b) && !used_bands.contains(b))
            .or_else(|| {
                radio.bands.first().map(|b| match b.as_str() {
                    "5g" => "5g",
                    "6g" => "6g",
                    _ => "2g",
                })
            });
        let Some(band) = band else { continue };
        let Some((channels, htmode)) = band_channel(band) else {
            continue;
        };
        used_bands.insert(band);
        let channel = *channels
            .iter()
            .min_by_key(|c| taken.iter().filter(|(b, ch)| b == band && ch == *c).count())
            .expect("channel list is not empty");
        taken.push((band.to_string(), channel));
        out.insert(
            radio.name.clone(),
            RadioPlan {
                band: band.to_string(),
                channel,
                htmode: htmode.to_string(),
            },
        );
    }
    out
}

/// What the Core's own radios occupy.
fn core_channels(wifi: &Wifi<ProfileId>) -> Vec<(String, u32)> {
    wifi.radios
        .values()
        .filter(|r| r.enabled)
        .filter_map(|r| Some((r.band.clone(), r.channel.parse().ok()?)))
        .collect()
}

// ── Payload ─────────────────────────────────────────────────────────────────

/// One access point's payload at `generation`.
pub fn build(
    inputs: &Inputs,
    sat: &PairedSatellite,
    radios: BTreeMap<String, RadioPlan>,
    generation: u64,
) -> SyncSnapshot {
    let known_ports: Option<BTreeSet<&str>> = sat.inventory.as_ref().map(|i| {
        i.ports
            .iter()
            .filter(|p| !p.uplink)
            .map(|p| p.name.as_str())
            .collect()
    });
    SyncSnapshot {
        generation,
        format: SYNC_FORMAT,
        ssid: inputs.wifi.ssid.clone(),
        broadcast_separately: inputs.wifi.broadcast_separately,
        country: inputs.wifi.country.clone(),
        radios,
        profiles: inputs
            .profiles
            .iter()
            .map(|p| ProfileSpec {
                fullname: p.fullname.clone(),
                interface: p.interface.clone(),
                vlan_tag: p.vlan_tag,
            })
            .collect(),
        passwords: inputs
            .wifi
            .passwords
            .iter()
            .map(|p| PasswordSpec {
                label: p.label.clone(),
                key: p.password.clone(),
                vlan_tag: p.profile.as_ref().map_or(ADMIN_VLAN, |pr| pr.vlan_tag),
            })
            .collect(),
        ports: sat
            .ports
            .iter()
            .filter(|(name, _)| {
                known_ports
                    .as_ref()
                    .map_or(true, |k| k.contains(name.as_str()))
            })
            .map(|(n, v)| (n.clone(), *v))
            .collect(),
        update_source: Some(inputs.update_source.clone()),
    }
}

/// Every access point's payload, generation 0, and a digest over all of them.
fn build_all(inputs: &Inputs, registry: &[PairedSatellite]) -> (Vec<SyncSnapshot>, String) {
    let mut taken = core_channels(&inputs.wifi);
    let mut snapshots = Vec::new();
    let mut hasher = blake3::Hasher::new();
    for sat in registry {
        let radios = sat
            .inventory
            .as_ref()
            .map(|i| plan(&i.radios, &taken))
            .unwrap_or_default();
        for p in radios.values() {
            taken.push((p.band.clone(), p.channel));
        }
        let snapshot = build(inputs, sat, radios, 0);
        hasher.update(sat.name.as_bytes());
        hasher.update(&serde_json::to_vec(&snapshot).expect("snapshot serializes"));
        snapshots.push(snapshot);
    }
    (snapshots, hasher.finalize().to_hex().to_string())
}

/// A generation never below what any access point has applied, so a Core
/// whose state file was lost does not push payloads they refuse as old.
pub fn floor_generation(generation: u64, registry: &[PairedSatellite]) -> u64 {
    registry
        .iter()
        .map(|s| s.generation_applied)
        .fold(generation, u64::max)
}

/// An access point reporting `format` reads the Core's payload; one reporting
/// none predates the number and reads format 1.
pub fn reads(format: u32) -> bool {
    format.max(1) >= SYNC_FORMAT
}

/// Drops a failed update's note once the access point no longer needs one.
fn clear_stale_failure(sat: &mut PairedSatellite) -> bool {
    let stale = sat
        .update
        .as_ref()
        .is_some_and(|u| u.state == ApUpdateState::Failed)
        && !crate::fleet_update::behind(sat, env!("CARGO_PKG_VERSION"));
    if stale {
        sat.update = None;
    }
    stale
}

/// Per-access-point push bookkeeping across ticks.
#[derive(Default)]
pub struct Pusher {
    last_attempt: BTreeMap<String, Instant>,
}

/// The current generation the Core announces, after this tick's rebuild.
pub fn current_generation() -> u64 {
    load_state().generation
}

/// One tick: rebuild, bump the generation if anything changed, read each live
/// access point's status, and push to those behind. `live` names the access
/// points whose tunnel is up. Returns whether the registry changed.
pub async fn tick(
    registry: &mut [PairedSatellite],
    live: &BTreeSet<String>,
    pusher: &mut Pusher,
) -> Result<bool, Error> {
    if registry.is_empty() {
        return Ok(false);
    }
    let inputs = gather().await?;
    let (snapshots, digest) = build_all(&inputs, registry);
    let mut state = load_state();
    state.generation = floor_generation(state.generation, registry);
    if state.digest != digest {
        state.generation += 1;
        state.digest = digest;
        save_state(&state).await?;
        tracing::info!(
            "sync: configuration changed; generation {}",
            state.generation
        );
    }
    let generation = state.generation;
    let mut changed = false;
    for (sat, mut snapshot) in registry.iter_mut().zip(snapshots) {
        if !live.contains(&sat.name) {
            continue;
        }
        let Ok(addr) = sat.mgmt_address.parse::<IpAddr>() else {
            continue;
        };
        match crate::control::status(addr, &sat.name, generation).await {
            Ok(status) => {
                if sat.generation_applied != status.generation {
                    sat.generation_applied = status.generation;
                    changed = true;
                }
                if sat.inventory.as_ref() != Some(&status.inventory) {
                    sat.inventory = Some(status.inventory);
                    changed = true;
                }
                if sat.clients != status.clients {
                    sat.clients = status.clients;
                    changed = true;
                }
                if sat.plan != snapshot.radios {
                    sat.plan = snapshot.radios.clone();
                    changed = true;
                }
                if sat.version != status.version {
                    sat.version = status.version;
                    changed = true;
                }
                if sat.recovery_ssid != status.recovery_ssid {
                    sat.recovery_ssid = status.recovery_ssid;
                    changed = true;
                }
                if sat.needs_update != status.needs_update {
                    sat.needs_update = status.needs_update;
                    changed = true;
                }
                if clear_stale_failure(sat) {
                    changed = true;
                }
                if !reads(status.format) {
                    continue;
                }
            }
            Err(e) => {
                tracing::debug!("sync: status of '{}': {e}", sat.name);
                continue;
            }
        }
        if sat.generation_applied >= generation {
            continue;
        }
        let now = Instant::now();
        if pusher
            .last_attempt
            .get(&sat.name)
            .is_some_and(|t| now.duration_since(*t) < RETRY_AFTER)
        {
            continue;
        }
        pusher.last_attempt.insert(sat.name.clone(), now);
        snapshot.generation = generation;
        match crate::control::push(addr, &snapshot).await {
            Ok(()) => {
                pusher.last_attempt.remove(&sat.name);
                sat.generation_applied = generation;
                changed = true;
                crate::activity::log(
                    "satellite",
                    "synced",
                    true,
                    &format!(
                        "Pushed configuration generation {generation} to satellite '{}'",
                        sat.name
                    ),
                    None,
                );
            }
            Err(e) => {
                tracing::warn!("sync: push to '{}' failed: {e}", sat.name);
                crate::activity::log(
                    "satellite",
                    "synced",
                    false,
                    &format!(
                        "Configuration generation {generation} did not reach satellite '{}'",
                        sat.name
                    ),
                    Some(&e.to_string()),
                );
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::satellite::{Inventory, PortInfo};
    use crate::wifi::{Password, WifiRadio};

    fn radio(name: &str, bands: &[&str]) -> RadioInfo {
        RadioInfo {
            name: name.into(),
            phy: "phy0".into(),
            bands: bands.iter().map(|b| b.to_string()).collect(),
        }
    }

    #[test]
    fn generation_never_falls_below_an_applied_one() {
        let mut s = sat();
        s.generation_applied = 11;
        assert_eq!(floor_generation(0, &[s.clone()]), 11);
        assert_eq!(floor_generation(12, &[s]), 12);
    }

    #[test]
    fn an_access_point_without_a_format_reads_format_one() {
        assert_eq!(reads(0), reads(1));
        assert!(reads(SYNC_FORMAT));
        assert!(reads(SYNC_FORMAT + 1));
        assert!(SYNC_FORMAT == 1 || !reads(SYNC_FORMAT - 1));
    }

    #[test]
    fn a_failed_update_clears_once_nothing_is_behind() {
        let failed = Some(crate::satellite::ApUpdate {
            target: env!("CARGO_PKG_VERSION").into(),
            state: ApUpdateState::Failed,
            message: Some("x".into()),
        });
        let mut s = sat();
        s.version = env!("CARGO_PKG_VERSION").into();
        s.update = failed.clone();
        s.needs_update = true;
        assert!(!clear_stale_failure(&mut s));
        s.needs_update = false;
        assert!(clear_stale_failure(&mut s));
        assert!(s.update.is_none());
        s.version = "0.0.1".into();
        s.update = failed;
        assert!(!clear_stale_failure(&mut s));
    }

    #[test]
    fn plan_spreads_bands_and_avoids_taken_channels() {
        let taken = vec![("2g".to_string(), 1), ("5g".to_string(), 36)];
        let p = plan(
            &[radio("radio0", &["2g"]), radio("radio1", &["5g"])],
            &taken,
        );
        assert_eq!((p["radio0"].band.as_str(), p["radio0"].channel), ("2g", 6));
        assert_eq!((p["radio1"].band.as_str(), p["radio1"].channel), ("5g", 40));
        assert_eq!(p["radio1"].htmode, "HE80");
        // One dual-band phy takes 2.4 GHz; a second radio on it takes 5 GHz.
        let p = plan(&[radio("radio0", &["2g", "5g"])], &[]);
        assert_eq!(p["radio0"].band, "2g");
        let p = plan(&[radio("a", &["2g", "5g"]), radio("b", &["2g", "5g"])], &[]);
        assert_eq!((p["a"].band.as_str(), p["b"].band.as_str()), ("2g", "5g"));
        // A radio with no known band gets no plan.
        assert!(plan(&[radio("x", &[])], &[]).is_empty());
    }

    fn inputs() -> Inputs {
        let admin = ProfileId {
            fullname: "Admin".into(),
            interface: "lan".into(),
            vlan_tag: 1,
        };
        let guest = ProfileId {
            fullname: "guest".into(),
            interface: "yyiph".into(),
            vlan_tag: 101,
        };
        let mut passwords = BTreeSet::new();
        passwords.insert(Password {
            label: "Default".into(),
            profile: None,
            password: "adminpw".into(),
        });
        passwords.insert(Password {
            label: "Guest".into(),
            profile: Some(guest.clone()),
            password: "guestpw".into(),
        });
        passwords.insert(Password {
            label: "Kids".into(),
            profile: Some(guest.clone()),
            password: "kidspw".into(),
        });
        let mut radios = BTreeMap::new();
        radios.insert(
            "radio0".to_string(),
            WifiRadio {
                band: "2g".into(),
                channel: "1".into(),
                width: Some(20),
                enabled: true,
                broadcast: true,
            },
        );
        Inputs {
            wifi: Wifi {
                ssid: "Home".into(),
                broadcast_separately: false,
                country: Some("US".into()),
                radios,
                passwords,
            },
            profiles: vec![admin, guest],
            update_source: "https://registry.example.com".into(),
        }
    }

    fn sat() -> PairedSatellite {
        PairedSatellite {
            name: "s1".into(),
            public_key: "k".into(),
            mgmt_address: "10.42.1.2".into(),
            ports: BTreeMap::from([("eth0".to_string(), Some(101)), ("eth9".to_string(), None)]),
            inventory: Some(Inventory {
                ports: vec![
                    PortInfo {
                        name: "eth0".into(),
                        uplink: false,
                        link: true,
                    },
                    PortInfo {
                        name: "eth1".into(),
                        uplink: true,
                        link: true,
                    },
                ],
                radios: vec![radio("radio0", &["2g", "5g"])],
            }),
            ..Default::default()
        }
    }

    #[test]
    fn payload_carries_every_password_with_its_vlan_and_only_known_ports() {
        let (snaps, digest) = build_all(&inputs(), &[sat()]);
        let s = &snaps[0];
        assert_eq!(s.generation, 0);
        assert_eq!(s.profiles.len(), 2);
        let mut keys: Vec<(String, u16)> = s
            .passwords
            .iter()
            .map(|p| (p.key.clone(), p.vlan_tag))
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                ("adminpw".into(), 1),
                ("guestpw".into(), 101),
                ("kidspw".into(), 101)
            ]
        );
        assert_eq!(s.ports, BTreeMap::from([("eth0".to_string(), Some(101))]));
        // The Core sits on 2.4 GHz channel 1, so the access point gets channel 6.
        assert_eq!(
            (s.radios["radio0"].band.as_str(), s.radios["radio0"].channel),
            ("2g", 6)
        );
        assert_eq!(
            s.update_source.as_deref(),
            Some("https://registry.example.com")
        );
        assert_eq!(digest.len(), 64);
        // Same inputs, same digest; a changed password, a new one.
        let (_, again) = build_all(&inputs(), &[sat()]);
        assert_eq!(digest, again);
        let mut i2 = inputs();
        i2.wifi.ssid = "Other".into();
        assert_ne!(build_all(&i2, &[sat()]).1, digest);
    }
}

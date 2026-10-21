//! Config sync (design D5, D20): the main router builds each access point's
//! payload from its own Wi-Fi, profiles and port assignments, numbers it, and
//! pushes it over the tunnel whenever it changes or an access point falls
//! behind.
//!
//! The payload is derived, never edited: every tick the main router rebuilds it
//! from its configs and compares a digest with the last one, so a password,
//! profile or port change reaches every access point within seconds with no
//! hook in the code that made the change. The generation is a counter the main
//! router keeps; an access point refuses anything older than what it applied.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::access_points::{
    AccessPoint, ApUpdateState, PasswordSpec, ProfileSpec, RadioInfo, RadioPlan, SyncSnapshot,
    SYNC_FORMAT,
};
use crate::ap::ADMIN_VLAN;
use crate::prelude::*;
use crate::profiles::ProfileId;
use crate::wifi::{ChannelBlock, Wifi};
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

/// The main router's side of every payload.
pub struct Inputs {
    pub wifi: Wifi<ProfileId>,
    pub profiles: Vec<ProfileId>,
    /// What the regulatory domain allows, per band.
    pub blocks: BTreeMap<String, Vec<ChannelBlock>>,
    /// (band, channel, width) of the main router's radios on the air.
    pub operating: Vec<(String, u32, u32)>,
}

async fn gather() -> Result<Inputs, Error> {
    let ctx = ServerContext::default();
    let wifi = crate::wifi::get(ctx.clone()).await?;
    let profiles = {
        let arena = uciedit::Arena::new();
        let cfgs = uciedit::parse_all("/etc/config", &arena, &["startwrt"]).await?;
        crate::profiles::list_config(ctx, &cfgs)?
    };
    let bands = wifi
        .radios
        .values()
        .filter(|r| r.enabled)
        .map(|r| r.band.clone())
        .collect();
    let operating = remember_operating(crate::wifi::operating_channels().await, &bands);
    let blocks = regulatory_blocks(wifi.country.as_deref()).await?;
    Ok(Inputs {
        wifi,
        profiles,
        blocks,
        operating,
    })
}

// ── Channel plan (D20) ──────────────────────────────────────────────────────

/// The widest channel an access point's radio is planned at.
fn planned_max_width(band: &str) -> u32 {
    if band == "2g" {
        20
    } else {
        80
    }
}

/// The frequencies a block occupies; a 2.4 GHz channel is 22 MHz wide.
fn span(block: &ChannelBlock) -> Option<(u32, u32)> {
    let half = if block.band == "2g" { 11 } else { 10 };
    let lo = crate::wifi::channel_mhz(&block.band, *block.channels.first()?)?;
    let hi = crate::wifi::channel_mhz(&block.band, *block.channels.last()?)?;
    Some((lo - half, hi + half))
}

fn overlaps(a: &ChannelBlock, b: &ChannelBlock) -> bool {
    a.band == b.band
        && matches!(
            (span(a), span(b)),
            (Some((al, ah)), Some((bl, bh))) if al < bh && bl < ah
        )
}

/// The widest block clear of every taken one, else the least shared block at
/// the widest width. A radar-detection channel is never chosen.
fn choose(band: &str, blocks: &[ChannelBlock], taken: &[ChannelBlock]) -> Option<ChannelBlock> {
    let allowed = |b: &&ChannelBlock| !b.radar && b.width <= planned_max_width(band);
    let mut widths: Vec<u32> = blocks.iter().filter(allowed).map(|b| b.width).collect();
    widths.sort_unstable_by(|a, b| b.cmp(a));
    widths.dedup();
    let at = |width: u32| {
        blocks
            .iter()
            .filter(allowed)
            .filter(move |b| b.width == width)
    };
    for &width in &widths {
        if let Some(free) = at(width).find(|b| !taken.iter().any(|t| overlaps(b, t))) {
            return Some(free.clone());
        }
    }
    at(*widths.first()?)
        .min_by_key(|b| taken.iter().filter(|t| overlaps(b, t)).count())
        .cloned()
}

/// Picks a band, channel and width for each of an access point's radios.
/// `blocks` is what the regulatory domain allows per band; `taken` is every
/// block in use on the main router and on access points planned before this
/// one, and gains this one's. The first radio of a dual-band phy takes 2.4 GHz
/// for reach; a second radio takes the other band.
pub fn plan(
    radios: &[RadioInfo],
    blocks: &BTreeMap<String, Vec<ChannelBlock>>,
    taken: &mut Vec<ChannelBlock>,
) -> BTreeMap<String, RadioPlan> {
    let mut out = BTreeMap::new();
    let mut used_bands: BTreeSet<&str> = BTreeSet::new();
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
        let Some(block) = blocks
            .get(band)
            .and_then(|options| choose(band, options, taken))
        else {
            continue;
        };
        used_bands.insert(band);
        out.insert(
            radio.name.clone(),
            RadioPlan {
                band: band.to_string(),
                channel: block.primary,
                htmode: format!("HE{}", block.width),
            },
        );
        taken.push(block);
    }
    out
}

/// What the main router's own radios occupy: as seen on the air, else as
/// configured.
fn main_blocks(wifi: &Wifi<ProfileId>, operating: &[(String, u32, u32)]) -> Vec<ChannelBlock> {
    let mut out: Vec<ChannelBlock> = operating
        .iter()
        .filter_map(|(band, channel, width)| crate::wifi::block_of(band, *channel, *width))
        .collect();
    for radio in wifi.radios.values().filter(|r| r.enabled) {
        if out.iter().any(|b| b.band == radio.band) {
            continue;
        }
        let Ok(channel) = radio.channel.parse() else {
            continue;
        };
        if let Some(block) = crate::wifi::block_of(&radio.band, channel, radio.width.unwrap_or(20))
        {
            out.push(block);
        }
    }
    out
}

/// The regulatory channel set changes only with the country.
async fn regulatory_blocks(
    country: Option<&str>,
) -> Result<BTreeMap<String, Vec<ChannelBlock>>, Error> {
    type Cached = Option<(Option<String>, BTreeMap<String, Vec<ChannelBlock>>)>;
    static CACHE: Mutex<Cached> = Mutex::new(None);
    if let Some((cached, blocks)) = &*CACHE.lock().unwrap() {
        if cached.as_deref() == country {
            return Ok(blocks.clone());
        }
    }
    let blocks = crate::wifi::channel_blocks().await?;
    *CACHE.lock().unwrap() = Some((country.map(str::to_string), blocks.clone()));
    Ok(blocks)
}

/// A radio that is off keeps its last channel in the plan, so a blackout
/// window moves no access point. `bands` are those with an enabled radio.
fn remember_operating(
    now: Vec<(String, u32, u32)>,
    bands: &BTreeSet<String>,
) -> Vec<(String, u32, u32)> {
    static LAST: Mutex<BTreeMap<String, (u32, u32)>> = Mutex::new(BTreeMap::new());
    let mut last = LAST.lock().unwrap();
    last.retain(|band, _| bands.contains(band));
    for (band, channel, width) in now {
        last.insert(band, (channel, width));
    }
    last.iter()
        .map(|(band, (channel, width))| (band.clone(), *channel, *width))
        .collect()
}

// ── Payload ─────────────────────────────────────────────────────────────────

/// One access point's payload at `generation`.
pub fn build(
    inputs: &Inputs,
    ap: &AccessPoint,
    radios: BTreeMap<String, RadioPlan>,
    generation: u64,
) -> SyncSnapshot {
    let known_ports: Option<BTreeSet<&str>> = ap.inventory.as_ref().map(|i| {
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
        ports: ap
            .ports
            .iter()
            .filter(|(name, _)| {
                known_ports
                    .as_ref()
                    .map_or(true, |k| k.contains(name.as_str()))
            })
            .map(|(n, v)| (n.clone(), *v))
            .collect(),
    }
}

/// Every access point's payload, generation 0, and a digest over all of them.
fn build_all(inputs: &Inputs, registry: &[AccessPoint]) -> (Vec<SyncSnapshot>, String) {
    let mut taken = main_blocks(&inputs.wifi, &inputs.operating);
    let mut snapshots = Vec::new();
    let mut hasher = blake3::Hasher::new();
    for ap in registry {
        let radios = ap
            .inventory
            .as_ref()
            .map(|i| plan(&i.radios, &inputs.blocks, &mut taken))
            .unwrap_or_default();
        let snapshot = build(inputs, ap, radios, 0);
        hasher.update(ap.name.as_bytes());
        hasher.update(&serde_json::to_vec(&snapshot).expect("snapshot serializes"));
        snapshots.push(snapshot);
    }
    (snapshots, hasher.finalize().to_hex().to_string())
}

/// A generation never below what any access point has applied, so a main router
/// whose state file was lost does not push payloads they refuse as old.
pub fn floor_generation(generation: u64, registry: &[AccessPoint]) -> u64 {
    registry
        .iter()
        .map(|s| s.generation_applied)
        .fold(generation, u64::max)
}

/// An access point reporting `format` reads the main router's payload; one
/// reporting none predates the number and reads format 1.
pub fn reads(format: u32) -> bool {
    format.max(1) >= SYNC_FORMAT
}

/// Drops a failed update's note once the access point no longer needs one.
fn clear_stale_failure(ap: &mut AccessPoint) -> bool {
    let stale = ap
        .update
        .as_ref()
        .is_some_and(|u| u.state == ApUpdateState::Failed)
        && !crate::fleet_update::behind(ap, env!("CARGO_PKG_VERSION"));
    if stale {
        ap.update = None;
    }
    stale
}

/// A push whose request failed; the access point may have applied it anyway.
struct Unconfirmed {
    generation: u64,
    error: String,
}

/// Per-access-point push bookkeeping across ticks.
#[derive(Default)]
pub struct Pusher {
    last_attempt: BTreeMap<String, Instant>,
    unconfirmed: BTreeMap<String, Unconfirmed>,
}

impl Pusher {
    /// Whether the last attempt is still within the retry delay.
    fn waiting(&self, name: &str, now: Instant) -> bool {
        self.last_attempt
            .get(name)
            .is_some_and(|t| now.duration_since(*t) < RETRY_AFTER)
    }

    fn failed(&mut self, name: &str, generation: u64, error: String) {
        self.unconfirmed
            .insert(name.to_string(), Unconfirmed { generation, error });
    }

    fn succeeded(&mut self, name: &str) {
        self.last_attempt.remove(name);
        self.unconfirmed.remove(name);
    }

    /// The generation an unconfirmed push turns out to have delivered.
    fn confirmed(&mut self, name: &str, applied: u64) -> Option<u64> {
        let generation = self.unconfirmed.get(name)?.generation;
        (applied >= generation).then(|| {
            self.succeeded(name);
            generation
        })
    }

    /// An unconfirmed push, taken back to be reported as failed.
    fn give_up(&mut self, name: &str) -> Option<Unconfirmed> {
        self.unconfirmed.remove(name)
    }
}

fn log_pushed(name: &str, generation: u64) {
    crate::activity::log(
        "access-point",
        "synced",
        true,
        &format!("Pushed configuration generation {generation} to access point '{name}'"),
        None,
    );
}

fn log_unreached(name: &str, generation: u64, error: &str) {
    tracing::warn!("sync: push to '{name}' failed: {error}");
    crate::activity::log(
        "access-point",
        "synced",
        false,
        &format!("Configuration generation {generation} did not reach access point '{name}'"),
        Some(error),
    );
}

/// The current generation the main router announces, after this tick's rebuild.
pub fn current_generation() -> u64 {
    load_state().generation
}

/// One tick: rebuild, bump the generation if anything changed, read each live
/// access point's status, and push to those behind. `live` names the access
/// points whose tunnel is up. Returns whether the registry changed.
pub async fn tick(
    registry: &mut [AccessPoint],
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
    for (ap, mut snapshot) in registry.iter_mut().zip(snapshots) {
        if !live.contains(&ap.name) {
            continue;
        }
        let Ok(addr) = ap.mgmt_address.parse::<IpAddr>() else {
            continue;
        };
        match crate::control::status(addr, &ap.name, generation).await {
            Ok(status) => {
                if ap.generation_applied != status.generation {
                    ap.generation_applied = status.generation;
                    changed = true;
                }
                if ap.inventory.as_ref() != Some(&status.inventory) {
                    ap.inventory = Some(status.inventory);
                    changed = true;
                }
                if ap.clients != status.clients {
                    ap.clients = status.clients;
                    changed = true;
                }
                if ap.plan != snapshot.radios {
                    ap.plan = snapshot.radios.clone();
                    changed = true;
                }
                if ap.version != status.version {
                    ap.version = status.version;
                    changed = true;
                }
                if ap.recovery_ssid != status.recovery_ssid {
                    ap.recovery_ssid = status.recovery_ssid;
                    changed = true;
                }
                if ap.needs_update != status.needs_update {
                    ap.needs_update = status.needs_update;
                    changed = true;
                }
                if ap.loop_blocked != status.loop_blocked {
                    ap.loop_blocked = status.loop_blocked;
                    changed = true;
                }
                if clear_stale_failure(ap) {
                    changed = true;
                }
                if let Some(delivered) = pusher.confirmed(&ap.name, ap.generation_applied) {
                    log_pushed(&ap.name, delivered);
                }
                if !reads(status.format) {
                    continue;
                }
            }
            Err(e) => {
                tracing::debug!("sync: status of '{}': {e}", ap.name);
                continue;
            }
        }
        if ap.generation_applied >= generation {
            continue;
        }
        let now = Instant::now();
        if pusher.waiting(&ap.name, now) {
            continue;
        }
        if let Some(lost) = pusher.give_up(&ap.name) {
            log_unreached(&ap.name, lost.generation, &lost.error);
        }
        pusher.last_attempt.insert(ap.name.clone(), now);
        snapshot.generation = generation;
        match crate::control::push(addr, &snapshot).await {
            Ok(()) => {
                pusher.succeeded(&ap.name);
                ap.generation_applied = generation;
                changed = true;
                log_pushed(&ap.name, generation);
            }
            Err(e) => {
                tracing::debug!("sync: push to '{}' unconfirmed: {e}", ap.name);
                pusher.failed(&ap.name, generation, e.to_string());
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access_points::{Inventory, PortInfo};
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
        let mut s = ap();
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
        let failed = Some(crate::access_points::ApUpdate {
            target: env!("CARGO_PKG_VERSION").into(),
            state: ApUpdateState::Failed,
            message: Some("x".into()),
        });
        let mut s = ap();
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

    fn us_blocks() -> BTreeMap<String, Vec<ChannelBlock>> {
        crate::wifi::blocks_from_iw(crate::wifi::IW_CHANNELS_US)
    }

    fn block(band: &str, primary: u32, width: u32) -> ChannelBlock {
        crate::wifi::block_of(band, primary, width).unwrap()
    }

    fn pick(p: &BTreeMap<String, RadioPlan>, radio: &str) -> (u32, String) {
        (p[radio].channel, p[radio].htmode.clone())
    }

    #[test]
    fn plan_takes_a_free_block_before_sharing_one() {
        let blocks = us_blocks();
        // The main router: 2.4 GHz channel 1, 5 GHz 36 at 80 MHz (36–48).
        let mut taken = vec![block("2g", 1, 20), block("5g", 36, 80)];
        let p = plan(
            &[radio("radio0", &["2g"]), radio("radio1", &["5g"])],
            &blocks,
            &mut taken,
        );
        assert_eq!(pick(&p, "radio0"), (7, "HE20".into()));
        assert_eq!(pick(&p, "radio1"), (149, "HE80".into()));
        assert_eq!(taken.len(), 4);
        // No 80 or 40 MHz block is clear of 36–48 and 149–161; 165 at 20 MHz is.
        let p = plan(&[radio("r", &["5g"])], &blocks, &mut taken);
        assert_eq!(pick(&p, "r"), (165, "HE20".into()));
        // Nothing is clear at any width: the least shared 80 MHz block, lowest first.
        let p = plan(&[radio("r", &["5g"])], &blocks, &mut taken);
        assert_eq!(pick(&p, "r"), (36, "HE80".into()));
    }

    #[test]
    fn plan_narrows_rather_than_share_and_skips_radar() {
        let blocks = us_blocks();
        // 52–64 is clear at 80 MHz but needs radar detection; 44–48 is clear at 40.
        let mut taken = vec![block("5g", 36, 40), block("5g", 149, 80)];
        let p = plan(&[radio("r", &["5g"])], &blocks, &mut taken);
        assert_eq!(pick(&p, "r"), (44, "HE40".into()));
    }

    #[test]
    fn plan_spreads_a_dual_band_phy_and_skips_unknown_bands() {
        let blocks = us_blocks();
        let p = plan(&[radio("radio0", &["2g", "5g"])], &blocks, &mut vec![]);
        assert_eq!(p["radio0"].band, "2g");
        let p = plan(
            &[radio("a", &["2g", "5g"]), radio("b", &["2g", "5g"])],
            &blocks,
            &mut vec![],
        );
        assert_eq!((p["a"].band.as_str(), p["b"].band.as_str()), ("2g", "5g"));
        assert!(plan(&[radio("x", &[])], &blocks, &mut vec![]).is_empty());
        assert!(plan(&[radio("x", &["6g"])], &blocks, &mut vec![]).is_empty());
    }

    #[test]
    fn two_point_four_ghz_plans_on_non_overlapping_channels() {
        let iw: String = std::iter::once("Band 1:\n".to_string())
            .chain((1..=11).map(|ch| {
                format!(
                    "\t* {} MHz [{ch}] \n\t  Channel widths: 20MHz\n",
                    2407 + 5 * ch
                )
            }))
            .collect();
        let blocks = crate::wifi::blocks_from_iw(&iw);
        let mut taken = Vec::new();
        let picks: Vec<u32> = (0..4)
            .map(|i| {
                let name = format!("r{i}");
                plan(&[radio(&name, &["2g"])], &blocks, &mut taken)[&name].channel
            })
            .collect();
        assert_eq!(picks, [1, 6, 11, 1]);
    }

    #[test]
    fn main_router_blocks_prefer_what_is_on_the_air() {
        let mut wifi = inputs().wifi;
        wifi.radios.insert(
            "radio1".into(),
            WifiRadio {
                band: "5g".into(),
                channel: "auto".into(),
                width: Some(80),
                enabled: true,
                broadcast: true,
            },
        );
        let seen = |blocks: Vec<ChannelBlock>| {
            blocks
                .iter()
                .map(|b| (b.band.clone(), b.primary, b.width))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            seen(main_blocks(&wifi, &[("5g".into(), 149, 80)])),
            [("5g".into(), 149, 80), ("2g".into(), 1, 20)]
        );
        // A radio on automatic selection that is not on the air occupies nothing.
        assert_eq!(
            seen(main_blocks(&wifi, &[("2g".into(), 6, 20)])),
            [("2g".into(), 6, 20)]
        );
    }

    #[test]
    fn a_radio_that_is_off_keeps_its_last_channel() {
        let bands: BTreeSet<String> = ["2g", "5g"].into_iter().map(String::from).collect();
        let seen = vec![("5g".to_string(), 149, 80)];
        assert_eq!(remember_operating(seen.clone(), &bands), seen);
        assert_eq!(remember_operating(vec![], &bands), seen);
        let only_2g: BTreeSet<String> = ["2g".to_string()].into();
        assert!(remember_operating(vec![], &only_2g).is_empty());
    }

    #[test]
    fn a_failed_push_is_confirmed_by_status_or_given_up_after_the_delay() {
        let mut p = Pusher::default();
        let t0 = Instant::now();
        p.last_attempt.insert("s1".into(), t0);
        p.failed("s1", 7, "timeout".into());
        assert!(p.waiting("s1", t0 + Duration::from_secs(5)));
        assert_eq!(p.confirmed("s1", 6), None);
        assert_eq!(p.confirmed("s1", 7), Some(7));
        assert!(!p.waiting("s1", t0 + Duration::from_secs(5)));
        assert!(p.give_up("s1").is_none());
        p.last_attempt.insert("s1".into(), t0);
        p.failed("s1", 8, "timeout".into());
        assert!(!p.waiting("s1", t0 + RETRY_AFTER));
        let lost = p.give_up("s1").unwrap();
        assert_eq!((lost.generation, lost.error.as_str()), (8, "timeout"));
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
            blocks: us_blocks(),
            operating: Vec::new(),
        }
    }

    fn ap() -> AccessPoint {
        AccessPoint {
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
        let (snaps, digest) = build_all(&inputs(), &[ap()]);
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
        // The main router sits on 2.4 GHz channel 1; 7 is the first channel in
        // the fixture clear of it.
        assert_eq!(
            (s.radios["radio0"].band.as_str(), s.radios["radio0"].channel),
            ("2g", 7)
        );
        assert_eq!(digest.len(), 64);
        // Same inputs, same digest; a changed password, a new one.
        let (_, again) = build_all(&inputs(), &[ap()]);
        assert_eq!(digest, again);
        let mut i2 = inputs();
        i2.wifi.ssid = "Other".into();
        assert_ne!(build_all(&i2, &[ap()]).1, digest);
    }
}

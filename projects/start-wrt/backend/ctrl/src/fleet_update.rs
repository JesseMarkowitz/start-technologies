//! Updates a main router's access points to its own version, one at a time
//! (design D17).
//!
//! The main router fetches each image from its own update source, checks it,
//! keeps it in RAM for the sequence, and sends it over the tunnel; an access
//! point reaches nothing beyond the main router. Each access point is waited
//! for, sent its image, and waited for again until it reports the new version;
//! only then does the next start, so the others' coverage overlaps the one
//! rebooting. The queue lives in the registry (`update` on each entry), so the
//! Access Points page shows it and a request made while a sequence runs joins
//! it.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

use crate::access_points::{self, AccessPoint, ApUpdate, ApUpdateState, RouterRole, TunnelStatus};
use crate::prelude::*;

const POLL: Duration = Duration::from_secs(5);
/// How long an access point may stay out of contact before its turn is skipped.
const CONTACT_WAIT: Duration = Duration::from_secs(10 * 60);
/// How long an access point may take to download, flash and come back.
const RETURN_WAIT: Duration = Duration::from_secs(15 * 60);
/// How long a main router stays up on its new version before it updates its
/// access points.
pub const SETTLE: Duration = Duration::from_secs(3 * 60);
/// Where the images live during a sequence: tmpfs.
const IMAGE_DIR: &str = "/tmp/startwrt-fleet";

static RUNNING: Mutex<bool> = Mutex::new(false);

fn target() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// It cannot read the main router's payload, or it runs an older version than
/// `target`.
pub fn behind(ap: &AccessPoint, target: &str) -> bool {
    ap.needs_update
        || (!ap.version.is_empty()
            && crate::system::cmp_versions(&ap.version, target) == Ordering::Less)
}

/// Marks `names`, or every access point behind `target` when empty, as queued.
/// One already queued or updating stays as it is. Returns the names it queued.
pub fn mark_queued(
    registry: &mut [AccessPoint],
    names: &[String],
    target: &str,
) -> Result<Vec<String>, Error> {
    for name in names {
        let Some(ap) = registry.iter().find(|s| s.name == *name) else {
            return Err(Error::new(
                eyre!("no adopted access point named '{name}'"),
                ErrorKind::NotFound,
            ));
        };
        if !behind(ap, target) {
            return Err(Error::new(
                eyre!(
                    "'{name}' runs v{}, not older than the main router's v{target}",
                    if ap.version.is_empty() {
                        "?"
                    } else {
                        &ap.version
                    }
                ),
                ErrorKind::InvalidRequest,
            ));
        }
    }
    let mut queued = Vec::new();
    for ap in registry.iter_mut() {
        let chosen = if names.is_empty() {
            behind(ap, target)
        } else {
            names.contains(&ap.name)
        };
        let pending = ap
            .update
            .as_ref()
            .is_some_and(|u| u.state != ApUpdateState::Failed);
        if !chosen || pending {
            continue;
        }
        ap.update = Some(ApUpdate {
            target: target.to_string(),
            state: ApUpdateState::Queued,
            message: None,
        });
        queued.push(ap.name.clone());
    }
    Ok(queued)
}

/// Queues access points and runs the sequence unless one is running.
pub async fn start(names: Vec<String>) -> Result<Vec<String>, Error> {
    let target = target();
    let queued = {
        let _guard = access_points::REGISTRY_LOCK.lock().await;
        let mut registry = access_points::load_registry();
        let queued = mark_queued(&mut registry, &names, target)?;
        if !queued.is_empty() {
            access_points::persist_registry(&registry).await?;
        }
        queued
    };
    if !queued.is_empty() {
        crate::activity::log(
            "access-point",
            "update",
            true,
            &format!(
                "Queued to update to v{target}, one at a time: {}",
                queued.join(", ")
            ),
            None,
        );
        ensure_running();
    }
    Ok(queued)
}

/// After the main router booted the version it updated to with "also update
/// access points": wait for it to settle, then update every access point behind
/// it.
pub fn after_main_update() {
    if access_points::load_role().role != RouterRole::Main {
        return;
    }
    std::thread::spawn(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fleet update runtime")
            .block_on(async {
                tokio::time::sleep(SETTLE).await;
                if let Err(e) = start(Vec::new()).await {
                    tracing::warn!("fleet update: access points not queued: {e}");
                }
            });
    });
}

fn ensure_running() {
    {
        let mut running = RUNNING.lock().unwrap();
        if *running {
            return;
        }
        *running = true;
    }
    if let Err(e) = std::thread::Builder::new()
        .name("fleet-update".into())
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("fleet update runtime")
                .block_on(run());
        })
    {
        tracing::error!("fleet update: failed to start thread: {e}");
        *RUNNING.lock().unwrap() = false;
    }
}

fn next_queued(registry: &[AccessPoint]) -> Option<String> {
    registry
        .iter()
        .find(|s| {
            s.update
                .as_ref()
                .is_some_and(|u| u.state == ApUpdateState::Queued)
        })
        .map(|s| s.name.clone())
}

fn failed(target: &str, message: String) -> Option<ApUpdate> {
    Some(ApUpdate {
        target: target.to_string(),
        state: ApUpdateState::Failed,
        message: Some(message),
    })
}

/// What a sequence works through: the registry, the access points and the
/// activity log.
trait Fleet {
    /// One board's image for the sequence.
    type Image;

    fn registry(&self) -> Vec<AccessPoint>;
    async fn set_update(&self, name: &str, update: Option<ApUpdate>);
    async fn status(&self, addr: IpAddr, name: &str) -> Result<TunnelStatus, Error>;
    async fn image(&mut self, platform: &str, from: &str) -> Result<Self::Image, String>;
    /// Sends the order, then the image; `Ok` once the access point has checked it.
    async fn deliver(&self, addr: IpAddr, target: &str, image: &Self::Image) -> Result<(), Error>;
    fn log(&self, success: bool, summary: &str, detail: Option<&str>);
    /// Whether the sequence may stop, with nothing queued.
    fn finish(&self) -> bool;
}

/// One image per board for the sequence, fetched when the first access point
/// on that board needs it.
struct Images {
    registry: String,
    target: String,
    fetched: BTreeMap<String, Result<(crate::update::Asset, PathBuf), String>>,
}

impl Images {
    async fn get(
        &mut self,
        platform: &str,
        from: &str,
    ) -> Result<(crate::update::Asset, PathBuf), String> {
        if let Some(r) = self.fetched.get(platform) {
            return r.clone();
        }
        let file: String = platform
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = PathBuf::from(IMAGE_DIR).join(format!("{}-{file}.bin", self.target));
        let r = async {
            tokio::fs::create_dir_all(IMAGE_DIR)
                .await
                .map_err(|e| format!("{IMAGE_DIR}: {e}"))?;
            crate::update::fetch_image(&self.registry, &self.target, platform, from, &path)
                .await
                .map_err(|e| format!("fetching the image for {platform}: {:#}", e.source))
        }
        .await
        .map(|asset| (asset, path));
        self.fetched.insert(platform.to_string(), r.clone());
        r
    }
}

impl Drop for Images {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(IMAGE_DIR);
    }
}

/// The registry on disk and the access points over their tunnels.
struct Live {
    images: Images,
}

impl Fleet for Live {
    type Image = (crate::update::Asset, PathBuf);

    fn registry(&self) -> Vec<AccessPoint> {
        access_points::load_registry()
    }

    async fn set_update(&self, name: &str, update: Option<ApUpdate>) {
        let _guard = access_points::REGISTRY_LOCK.lock().await;
        let mut registry = access_points::load_registry();
        if let Some(ap) = registry.iter_mut().find(|s| s.name == name) {
            ap.update = update;
            if let Err(e) = access_points::persist_registry(&registry).await {
                tracing::error!("fleet update: registry not saved: {e}");
            }
        }
    }

    async fn status(&self, addr: IpAddr, name: &str) -> Result<TunnelStatus, Error> {
        crate::control::status(addr, name, crate::sync::current_generation()).await
    }

    async fn image(&mut self, platform: &str, from: &str) -> Result<Self::Image, String> {
        self.images.get(platform, from).await
    }

    async fn deliver(
        &self,
        addr: IpAddr,
        target: &str,
        (asset, image): &Self::Image,
    ) -> Result<(), Error> {
        crate::control::update(addr, target, asset).await?;
        crate::control::send_firmware(addr, image).await
    }

    fn log(&self, success: bool, summary: &str, detail: Option<&str>) {
        crate::activity::log("access-point", "update", success, summary, detail);
    }

    fn finish(&self) -> bool {
        let mut running = RUNNING.lock().unwrap();
        if next_queued(&self.registry()).is_some() {
            return false;
        }
        *running = false;
        true
    }
}

async fn run() {
    let target = target();
    let mut fleet = Live {
        images: Images {
            registry: crate::system::read_registry_url()
                .unwrap_or_else(|| crate::system::DEFAULT_REGISTRY_URL.to_string()),
            target: target.to_string(),
            fetched: BTreeMap::new(),
        },
    };
    sequence(&mut fleet, target).await;
}

/// Works through the queued access points one at a time until none is left.
async fn sequence<F: Fleet>(fleet: &mut F, target: &str) {
    loop {
        let Some(name) = next_queued(&fleet.registry()) else {
            if fleet.finish() {
                return;
            }
            continue;
        };
        match update_one(fleet, &name, target).await {
            Ok(()) => {
                fleet.set_update(&name, None).await;
                fleet.log(true, &format!("Access point '{name}' runs v{target}"), None);
            }
            Err(Stop::Skip(message)) => {
                fleet.log(
                    false,
                    &format!("Access point '{name}' was not updated"),
                    Some(&message),
                );
                fleet.set_update(&name, failed(target, message)).await;
            }
            Err(Stop::Halt(message)) => {
                fleet.log(
                    false,
                    &format!(
                        "Access point '{name}' did not come back on v{target}; the access points after it were not updated"
                    ),
                    Some(&message),
                );
                fleet.set_update(&name, failed(target, message)).await;
                while let Some(other) = next_queued(&fleet.registry()) {
                    fleet
                        .set_update(
                            &other,
                            failed(target, format!("not started: '{name}' did not come back")),
                        )
                        .await;
                }
            }
        }
    }
}

/// On the main router's version or newer, and able to read its configuration.
fn caught_up(version: &str, needs_update: bool, target: &str) -> bool {
    !needs_update && crate::system::cmp_versions(version, target) != Ordering::Less
}

enum Stop {
    /// Nothing changed on it; the sequence moves on.
    Skip(String),
    /// It may be down; the sequence stops.
    Halt(String),
}

async fn update_one<F: Fleet>(fleet: &mut F, name: &str, target: &str) -> Result<(), Stop> {
    let deadline = Instant::now() + CONTACT_WAIT;
    let (addr, version): (IpAddr, String) = loop {
        let Some(ap) = fleet.registry().into_iter().find(|s| s.name == name) else {
            return Err(Stop::Skip("removed from the main router".into()));
        };
        if !behind(&ap, target) {
            return Ok(());
        }
        if ap.live {
            if let Ok(addr) = ap.mgmt_address.parse() {
                break (addr, ap.version);
            }
        }
        if Instant::now() >= deadline {
            return Err(Stop::Skip("out of contact".into()));
        }
        tokio::time::sleep(POLL).await;
    };
    let platform = fleet
        .status(addr, name)
        .await
        .map(|s| s.platform)
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(crate::registry::device_info::detect_platform);
    let image = fleet.image(&platform, &version).await.map_err(Stop::Skip)?;
    fleet
        .set_update(
            name,
            Some(ApUpdate {
                target: target.to_string(),
                state: ApUpdateState::Updating,
                message: None,
            }),
        )
        .await;
    fleet
        .deliver(addr, target, &image)
        .await
        .map_err(|e| Stop::Skip(format!("{:#}", e.source)))?;
    let deadline = Instant::now() + RETURN_WAIT;
    loop {
        tokio::time::sleep(POLL).await;
        if let Ok(status) = fleet.status(addr, name).await {
            if caught_up(&status.version, status.needs_update, target) {
                return Ok(());
            }
            match status.update {
                Some(u) if u.state == ApUpdateState::Failed => {
                    return Err(Stop::Skip(
                        u.message.unwrap_or_else(|| "the update failed".into()),
                    ));
                }
                Some(_) => {}
                // Its update state is held in memory, so none means it restarted.
                None if status.needs_update => {
                    return Err(Stop::Halt(format!(
                        "restarted on v{} and still cannot read the main router's configuration",
                        status.version
                    )));
                }
                None => {
                    return Err(Stop::Halt(format!(
                        "restarted on v{} instead of v{target}",
                        status.version
                    )));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(Stop::Halt(format!("did not come back on v{target}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ap(name: &str, version: &str) -> AccessPoint {
        AccessPoint {
            name: name.into(),
            version: version.into(),
            ..Default::default()
        }
    }

    #[test]
    fn behind_means_older_or_unable_to_read() {
        assert!(behind(&ap("a", "1.3.0"), "1.3.1"));
        assert!(!behind(&ap("a", "1.3.1"), "1.3.1"));
        assert!(!behind(&ap("a", "1.4.0"), "1.3.1"));
        assert!(!behind(&ap("a", ""), "1.3.1"));
        let mut s = ap("a", "1.3.1");
        s.needs_update = true;
        assert!(behind(&s, "1.3.1"));
    }

    #[test]
    fn caught_up_needs_the_version_and_a_readable_configuration() {
        assert!(caught_up("1.3.1", false, "1.3.1"));
        assert!(caught_up("1.4.0", false, "1.3.1"));
        assert!(!caught_up("1.3.0", false, "1.3.1"));
        assert!(!caught_up("1.3.1", true, "1.3.1"));
    }

    #[test]
    fn all_queues_only_those_behind_and_not_already_pending() {
        let mut reg = vec![ap("a", "1.3.0"), ap("b", "1.3.1"), ap("c", "1.2.0")];
        reg[2].update = Some(ApUpdate {
            target: "1.3.1".into(),
            state: ApUpdateState::Updating,
            message: None,
        });
        let q = mark_queued(&mut reg, &[], "1.3.1").unwrap();
        assert_eq!(q, vec!["a"]);
        assert_eq!(reg[0].update.as_ref().unwrap().state, ApUpdateState::Queued);
        assert!(reg[1].update.is_none());
        assert_eq!(
            reg[2].update.as_ref().unwrap().state,
            ApUpdateState::Updating
        );
        // A failed one is queued again.
        reg[2].update.as_mut().unwrap().state = ApUpdateState::Failed;
        assert_eq!(mark_queued(&mut reg, &[], "1.3.1").unwrap(), vec!["c"]);
    }

    #[test]
    fn a_named_access_point_must_exist_and_be_behind() {
        let mut reg = vec![ap("a", "1.3.0"), ap("b", "1.3.1")];
        assert!(mark_queued(&mut reg, &["x".into()], "1.3.1").is_err());
        assert!(mark_queued(&mut reg, &["b".into()], "1.3.1").is_err());
        assert!(reg.iter().all(|s| s.update.is_none()));
        assert_eq!(
            mark_queued(&mut reg, &["a".into()], "1.3.1").unwrap(),
            vec!["a"]
        );
    }

    /// How a simulated access point behaves once ordered to update.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Script {
        Good,
        FailsBeforeReboot,
        RejectsImage,
        ComesBackOld,
        ComesBackUnreadable,
        NeverReturns,
        OutOfContact,
    }

    /// Writing the image before the reboot, then the reboot itself.
    const FLASH: Duration = Duration::from_secs(10);
    const REBOOT: Duration = Duration::from_secs(60);

    struct SimAp {
        name: String,
        addr: IpAddr,
        platform: String,
        script: Script,
    }

    struct Sim {
        registry: Mutex<Vec<AccessPoint>>,
        aps: Vec<SimAp>,
        /// Each image sent, in order, with when.
        delivered: Mutex<Vec<(String, Instant)>>,
        returned: Mutex<BTreeMap<String, Instant>>,
        broken_platforms: Vec<String>,
        log: Mutex<Vec<(bool, String, Option<String>)>>,
    }

    const FROM: &str = "1.3.0";
    const TO: &str = "1.3.1";

    impl Sim {
        fn new(aps: &[(&str, &str, Script)]) -> Self {
            let mut registry = Vec::new();
            let mut sims = Vec::new();
            for (i, (name, platform, script)) in aps.iter().enumerate() {
                let addr: IpAddr = format!("10.42.{}.2", i + 1).parse().unwrap();
                registry.push(AccessPoint {
                    name: name.to_string(),
                    mgmt_address: addr.to_string(),
                    version: FROM.into(),
                    live: *script != Script::OutOfContact,
                    ..Default::default()
                });
                sims.push(SimAp {
                    name: name.to_string(),
                    addr,
                    platform: platform.to_string(),
                    script: *script,
                });
            }
            mark_queued(&mut registry, &[], TO).unwrap();
            Sim {
                registry: Mutex::new(registry),
                aps: sims,
                delivered: Mutex::new(Vec::new()),
                returned: Mutex::new(BTreeMap::new()),
                broken_platforms: Vec::new(),
                log: Mutex::new(Vec::new()),
            }
        }

        fn ap(&self, addr: IpAddr) -> &SimAp {
            self.aps.iter().find(|a| a.addr == addr).unwrap()
        }

        fn delivered_at(&self, name: &str) -> Option<Instant> {
            self.delivered
                .lock()
                .unwrap()
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, t)| *t)
        }

        fn update_of(&self, name: &str) -> Option<ApUpdate> {
            self.registry
                .lock()
                .unwrap()
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .update
                .clone()
        }

        fn failure(&self, name: &str) -> Option<String> {
            self.update_of(name)
                .filter(|u| u.state == ApUpdateState::Failed)
                .and_then(|u| u.message)
        }

        fn delivered_names(&self) -> Vec<String> {
            self.delivered
                .lock()
                .unwrap()
                .iter()
                .map(|(n, _)| n.clone())
                .collect()
        }

        /// Each image went out only once the one before it was back.
        fn assert_one_at_a_time(&self) {
            let delivered = self.delivered.lock().unwrap();
            let returned = self.returned.lock().unwrap();
            for pair in delivered.windows(2) {
                if let Some(back) = returned.get(&pair[0].0) {
                    assert!(
                        pair[1].1 >= *back,
                        "{} started before {} was back",
                        pair[1].0,
                        pair[0].0
                    );
                }
            }
        }
    }

    fn unreachable() -> Error {
        Error::new(eyre!("no route to host"), ErrorKind::Network)
    }

    impl Fleet for Sim {
        type Image = String;

        fn registry(&self) -> Vec<AccessPoint> {
            self.registry.lock().unwrap().clone()
        }

        async fn set_update(&self, name: &str, update: Option<ApUpdate>) {
            let mut registry = self.registry.lock().unwrap();
            if let Some(ap) = registry.iter_mut().find(|s| s.name == name) {
                ap.update = update;
            }
        }

        async fn status(&self, addr: IpAddr, _name: &str) -> Result<TunnelStatus, Error> {
            let ap = self.ap(addr);
            let mut status = TunnelStatus {
                version: FROM.into(),
                platform: ap.platform.clone(),
                ..Default::default()
            };
            let Some(at) = self.delivered_at(&ap.name) else {
                return Ok(status);
            };
            let updating = Some(ApUpdate {
                target: TO.into(),
                state: ApUpdateState::Updating,
                message: None,
            });
            let elapsed = Instant::now() - at;
            if ap.script == Script::FailsBeforeReboot {
                status.update = if elapsed < FLASH {
                    updating
                } else {
                    failed(TO, "sysupgrade: image check failed".into())
                };
                return Ok(status);
            }
            if elapsed < FLASH {
                status.update = updating;
                return Ok(status);
            }
            if elapsed < FLASH + REBOOT || ap.script == Script::NeverReturns {
                return Err(unreachable());
            }
            match ap.script {
                Script::ComesBackOld => {}
                Script::ComesBackUnreadable => {
                    status.version = TO.into();
                    status.needs_update = true;
                }
                _ => status.version = TO.into(),
            }
            self.returned
                .lock()
                .unwrap()
                .entry(ap.name.clone())
                .or_insert_with(Instant::now);
            Ok(status)
        }

        async fn image(&mut self, platform: &str, _from: &str) -> Result<String, String> {
            if self.broken_platforms.iter().any(|p| p == platform) {
                return Err(format!("fetching the image for {platform}: 404 Not Found"));
            }
            Ok(platform.to_string())
        }

        async fn deliver(&self, addr: IpAddr, target: &str, image: &String) -> Result<(), Error> {
            assert_eq!(target, TO);
            let ap = self.ap(addr);
            assert_eq!(*image, ap.platform, "wrong board's image");
            if ap.script == Script::RejectsImage {
                return Err(Error::new(
                    eyre!("{addr} refused the image: 400 Bad Request: the image does not match its commitment"),
                    ErrorKind::Network,
                ));
            }
            self.delivered
                .lock()
                .unwrap()
                .push((ap.name.clone(), Instant::now()));
            Ok(())
        }

        fn log(&self, success: bool, summary: &str, detail: Option<&str>) {
            self.log
                .lock()
                .unwrap()
                .push((success, summary.into(), detail.map(Into::into)));
        }

        fn finish(&self) -> bool {
            next_queued(&self.registry()).is_none()
        }
    }

    const K1: &str = "spacemit,k1-x";
    const X86: &str = "x86-64";

    async fn simulate(sim: &mut Sim) -> Duration {
        let start = Instant::now();
        sequence(sim, TO).await;
        Instant::now() - start
    }

    #[tokio::test(start_paused = true)]
    async fn five_access_points_update_one_at_a_time() {
        use Script::*;
        let mut sim = Sim::new(&[
            ("a", K1, Good),
            ("b", K1, Good),
            ("c", X86, Good),
            ("d", K1, Good),
            ("e", X86, Good),
        ]);
        let took = simulate(&mut sim).await;
        assert_eq!(sim.delivered_names(), ["a", "b", "c", "d", "e"]);
        sim.assert_one_at_a_time();
        for name in ["a", "b", "c", "d", "e"] {
            assert_eq!(sim.update_of(name), None, "{name}");
        }
        let log = sim.log.lock().unwrap();
        assert_eq!(log.len(), 5);
        assert!(log
            .iter()
            .all(|(ok, summary, _)| *ok && summary.ends_with("runs v1.3.1")));
        assert!(took >= 5 * (FLASH + REBOOT));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_that_leaves_the_access_point_unchanged_skips_it() {
        use Script::*;
        let mut sim = Sim::new(&[
            ("a", K1, Good),
            ("b", K1, FailsBeforeReboot),
            ("c", K1, OutOfContact),
            ("d", K1, RejectsImage),
            ("e", K1, Good),
        ]);
        let took = simulate(&mut sim).await;
        assert_eq!(sim.update_of("a"), None);
        assert_eq!(
            sim.failure("b").as_deref(),
            Some("sysupgrade: image check failed")
        );
        assert_eq!(sim.failure("c").as_deref(), Some("out of contact"));
        assert!(sim.failure("d").unwrap().contains("refused the image"));
        assert_eq!(sim.update_of("e"), None);
        assert_eq!(sim.delivered_names(), ["a", "b", "e"]);
        sim.assert_one_at_a_time();
        assert!(took >= CONTACT_WAIT);
    }

    #[tokio::test(start_paused = true)]
    async fn one_back_on_the_old_version_stops_the_rest() {
        use Script::*;
        let mut sim = Sim::new(&[
            ("a", K1, Good),
            ("b", K1, ComesBackOld),
            ("c", K1, Good),
            ("d", K1, Good),
        ]);
        simulate(&mut sim).await;
        assert_eq!(sim.update_of("a"), None);
        assert_eq!(
            sim.failure("b").as_deref(),
            Some("restarted on v1.3.0 instead of v1.3.1")
        );
        for name in ["c", "d"] {
            assert_eq!(
                sim.failure(name).as_deref(),
                Some("not started: 'b' did not come back")
            );
        }
        assert_eq!(sim.delivered_names(), ["a", "b"]);
    }

    #[tokio::test(start_paused = true)]
    async fn one_back_unable_to_read_the_configuration_stops_the_rest() {
        use Script::*;
        let mut sim = Sim::new(&[("a", K1, ComesBackUnreadable), ("b", K1, Good)]);
        simulate(&mut sim).await;
        assert!(sim
            .failure("a")
            .unwrap()
            .contains("still cannot read the main router's configuration"));
        assert_eq!(
            sim.failure("b").as_deref(),
            Some("not started: 'a' did not come back")
        );
        assert_eq!(sim.delivered_names(), ["a"]);
    }

    #[tokio::test(start_paused = true)]
    async fn one_that_never_returns_stops_the_rest_after_the_wait() {
        use Script::*;
        let mut sim = Sim::new(&[("a", K1, NeverReturns), ("b", K1, Good)]);
        let took = simulate(&mut sim).await;
        assert_eq!(
            sim.failure("a").as_deref(),
            Some("did not come back on v1.3.1")
        );
        assert_eq!(
            sim.failure("b").as_deref(),
            Some("not started: 'a' did not come back")
        );
        assert_eq!(sim.delivered_names(), ["a"]);
        assert!(took >= RETURN_WAIT);
        let log = sim.log.lock().unwrap();
        assert!(log[0]
            .1
            .contains("the access points after it were not updated"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_board_without_an_image_is_skipped_and_the_rest_continue() {
        use Script::*;
        let mut sim = Sim::new(&[("a", X86, Good), ("b", K1, Good), ("c", X86, Good)]);
        sim.broken_platforms = vec![X86.into()];
        simulate(&mut sim).await;
        assert!(sim.failure("a").unwrap().contains("404 Not Found"));
        assert_eq!(sim.update_of("b"), None);
        assert!(sim.failure("c").unwrap().contains("404 Not Found"));
        assert_eq!(sim.delivered_names(), ["b"]);
    }
}

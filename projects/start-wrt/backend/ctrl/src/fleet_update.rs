//! Updates a Core's access points to the Core's own version, one at a time
//! (design D17).
//!
//! Each access point is waited for, told to update over its tunnel, and waited
//! for again until it reports the new version; only then does the next start,
//! so the others' coverage overlaps the one rebooting. The queue lives in the
//! registry (`update` on each entry), so the Access Points page shows it and a
//! request made while a sequence runs joins it.

use std::cmp::Ordering;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::prelude::*;
use crate::satellite::{self, ApUpdate, ApUpdateState, PairedSatellite, RouterRole};

const POLL: Duration = Duration::from_secs(5);
/// How long an access point may stay out of contact before its turn is skipped.
const CONTACT_WAIT: Duration = Duration::from_secs(10 * 60);
/// How long an access point may take to download, flash and come back.
const RETURN_WAIT: Duration = Duration::from_secs(15 * 60);
/// How long a Core stays up on its new version before it updates its access points.
pub const SETTLE: Duration = Duration::from_secs(3 * 60);

static RUNNING: Mutex<bool> = Mutex::new(false);

fn target() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// It cannot read the Core's payload, or it runs an older version than `target`.
pub fn behind(sat: &PairedSatellite, target: &str) -> bool {
    sat.needs_update
        || (!sat.version.is_empty()
            && crate::system::cmp_versions(&sat.version, target) == Ordering::Less)
}

/// Marks `names`, or every access point behind `target` when empty, as queued.
/// One already queued or updating stays as it is. Returns the names it queued.
pub fn mark_queued(
    registry: &mut [PairedSatellite],
    names: &[String],
    target: &str,
) -> Result<Vec<String>, Error> {
    for name in names {
        let Some(sat) = registry.iter().find(|s| s.name == *name) else {
            return Err(Error::new(
                eyre!("no adopted satellite named '{name}'"),
                ErrorKind::NotFound,
            ));
        };
        if !behind(sat, target) {
            return Err(Error::new(
                eyre!(
                    "'{name}' runs v{}, not older than the Core's v{target}",
                    if sat.version.is_empty() {
                        "?"
                    } else {
                        &sat.version
                    }
                ),
                ErrorKind::InvalidRequest,
            ));
        }
    }
    let mut queued = Vec::new();
    for sat in registry.iter_mut() {
        let chosen = if names.is_empty() {
            behind(sat, target)
        } else {
            names.contains(&sat.name)
        };
        let pending = sat
            .update
            .as_ref()
            .is_some_and(|u| u.state != ApUpdateState::Failed);
        if !chosen || pending {
            continue;
        }
        sat.update = Some(ApUpdate {
            target: target.to_string(),
            state: ApUpdateState::Queued,
            message: None,
        });
        queued.push(sat.name.clone());
    }
    Ok(queued)
}

/// Queues access points and runs the sequence unless one is running.
pub async fn start(names: Vec<String>) -> Result<Vec<String>, Error> {
    let target = target();
    let queued = {
        let _guard = satellite::REGISTRY_LOCK.lock().await;
        let mut registry = satellite::load_registry();
        let queued = mark_queued(&mut registry, &names, target)?;
        if !queued.is_empty() {
            satellite::persist_registry(&registry).await?;
        }
        queued
    };
    if !queued.is_empty() {
        crate::activity::log(
            "satellite",
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

/// After the Core booted the version it updated to with "also update access
/// points": wait for it to settle, then update every access point behind it.
pub fn after_core_update() {
    if satellite::load_role().role != RouterRole::Core {
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

fn next_queued() -> Option<String> {
    satellite::load_registry()
        .into_iter()
        .find(|s| {
            s.update
                .as_ref()
                .is_some_and(|u| u.state == ApUpdateState::Queued)
        })
        .map(|s| s.name)
}

async fn set_update(name: &str, update: Option<ApUpdate>) {
    let _guard = satellite::REGISTRY_LOCK.lock().await;
    let mut registry = satellite::load_registry();
    if let Some(sat) = registry.iter_mut().find(|s| s.name == name) {
        sat.update = update;
        if let Err(e) = satellite::persist_registry(&registry).await {
            tracing::error!("fleet update: registry not saved: {e}");
        }
    }
}

fn failed(target: &str, message: String) -> Option<ApUpdate> {
    Some(ApUpdate {
        target: target.to_string(),
        state: ApUpdateState::Failed,
        message: Some(message),
    })
}

async fn run() {
    let registry_url = crate::system::read_registry_url()
        .unwrap_or_else(|| crate::system::DEFAULT_REGISTRY_URL.to_string());
    let target = target();
    loop {
        let Some(name) = next_queued() else {
            let mut running = RUNNING.lock().unwrap();
            if next_queued().is_none() {
                *running = false;
                return;
            }
            continue;
        };
        match update_one(&name, &registry_url, target).await {
            Ok(()) => {
                set_update(&name, None).await;
                crate::activity::log(
                    "satellite",
                    "update",
                    true,
                    &format!("Satellite '{name}' runs v{target}"),
                    None,
                );
            }
            Err(Stop::Skip(message)) => {
                crate::activity::log(
                    "satellite",
                    "update",
                    false,
                    &format!("Satellite '{name}' was not updated"),
                    Some(&message),
                );
                set_update(&name, failed(target, message)).await;
            }
            Err(Stop::Halt(message)) => {
                crate::activity::log(
                    "satellite",
                    "update",
                    false,
                    &format!(
                        "Satellite '{name}' did not come back on v{target}; the access points after it were not updated"
                    ),
                    Some(&message),
                );
                set_update(&name, failed(target, message)).await;
                while let Some(other) = next_queued() {
                    set_update(
                        &other,
                        failed(target, format!("not started: '{name}' did not come back")),
                    )
                    .await;
                }
            }
        }
    }
}

/// On the Core's version or newer, and able to read its configuration.
fn caught_up(version: &str, needs_update: bool, target: &str) -> bool {
    !needs_update && crate::system::cmp_versions(version, target) != Ordering::Less
}

enum Stop {
    /// Nothing changed on it; the sequence moves on.
    Skip(String),
    /// It may be down; the sequence stops.
    Halt(String),
}

fn find(name: &str) -> Option<PairedSatellite> {
    satellite::load_registry()
        .into_iter()
        .find(|s| s.name == name)
}

async fn update_one(name: &str, registry: &str, target: &str) -> Result<(), Stop> {
    let deadline = Instant::now() + CONTACT_WAIT;
    let addr: IpAddr = loop {
        let Some(sat) = find(name) else {
            return Err(Stop::Skip("removed from the Core".into()));
        };
        if !behind(&sat, target) {
            return Ok(());
        }
        if sat.live {
            if let Ok(addr) = sat.mgmt_address.parse() {
                break addr;
            }
        }
        if Instant::now() >= deadline {
            return Err(Stop::Skip("out of contact".into()));
        }
        tokio::time::sleep(POLL).await;
    };
    set_update(
        name,
        Some(ApUpdate {
            target: target.to_string(),
            state: ApUpdateState::Updating,
            message: None,
        }),
    )
    .await;
    crate::control::update(addr, registry, target)
        .await
        .map_err(|e| Stop::Skip(e.to_string()))?;
    let deadline = Instant::now() + RETURN_WAIT;
    loop {
        tokio::time::sleep(POLL).await;
        if let Ok(status) =
            crate::control::status(addr, name, crate::sync::current_generation()).await
        {
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
                        "restarted on v{} and still cannot read the Core's configuration",
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

    fn sat(name: &str, version: &str) -> PairedSatellite {
        PairedSatellite {
            name: name.into(),
            version: version.into(),
            ..Default::default()
        }
    }

    #[test]
    fn behind_means_older_or_unable_to_read() {
        assert!(behind(&sat("a", "1.3.0"), "1.3.1"));
        assert!(!behind(&sat("a", "1.3.1"), "1.3.1"));
        assert!(!behind(&sat("a", "1.4.0"), "1.3.1"));
        assert!(!behind(&sat("a", ""), "1.3.1"));
        let mut s = sat("a", "1.3.1");
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
        let mut reg = vec![sat("a", "1.3.0"), sat("b", "1.3.1"), sat("c", "1.2.0")];
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
        let mut reg = vec![sat("a", "1.3.0"), sat("b", "1.3.1")];
        assert!(mark_queued(&mut reg, &["x".into()], "1.3.1").is_err());
        assert!(mark_queued(&mut reg, &["b".into()], "1.3.1").is_err());
        assert!(reg.iter().all(|s| s.update.is_none()));
        assert_eq!(
            mark_queued(&mut reg, &["a".into()], "1.3.1").unwrap(),
            vec!["a"]
        );
    }
}

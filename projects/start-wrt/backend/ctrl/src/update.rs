use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::continuations::{Guid, RpcContinuation};
use crate::error::Error;
use crate::progress::{FullProgressTracker, ProgressUnits};
use crate::registry::device_info::DeviceInfo;
use crate::registry::os::{OsVersionInfo, SIG_CONTEXT};
use crate::registry::{self};
use crate::ServerContext;

// ── Request / Response types ─────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize, clap::Parser, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
#[group(skip)]
pub struct UpdateSystemParams {
    #[arg(long)]
    pub registry: Option<String>,
    #[arg(long)]
    pub target_version: Option<String>,
    /// On a Core, update its access points to the same version once it is back.
    #[arg(long)]
    #[serde(default)]
    pub update_access_points: bool,
}
rpc_toolkit::reflect_ts!(UpdateSystemParams);

#[derive(Debug, Serialize, Deserialize, visit_rs::VisitFields)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSystemRes {
    pub target: Option<String>,
    pub progress: Option<String>,
}
rpc_toolkit::reflect_ts!(UpdateSystemRes);

// ── Handlers ─────────────────────────────────────────────────────────

/// Check the registry for newer OS versions.
/// Used by both `system.newer-versions` and `system.update`.
pub async fn fetch_newer_versions(
    registry_url: &str,
) -> Result<BTreeMap<String, OsVersionInfo>, Error> {
    let device_info = DeviceInfo::load()?;
    let device_info_header = device_info.to_header_value()?;
    let client = reqwest::Client::new();

    let current_version = env!("CARGO_PKG_VERSION");
    let params = serde_json::json!({
        "sourceVersion": current_version,
    });

    let result = registry::call_registry_rpc(
        &client,
        registry_url,
        "os.version.get",
        params,
        Some(device_info_header),
    )
    .await?;

    let versions: BTreeMap<String, OsVersionInfo> = serde_json::from_value(result)
        .map_err(|e| Error::other(format!("parsing registry version response: {e}")))?;

    Ok(versions)
}

type Asset = crate::registry::asset::RegistryAsset<crate::sign::commitment::Blake3Commitment>;

/// The version to install, `target` or else the newest offered, and its
/// signature-checked image for this platform. `None` when nothing newer is offered.
async fn select(registry: &str, target: Option<&str>) -> Result<Option<(String, Asset)>, Error> {
    let platform = DeviceInfo::load()?.os.platform;
    let versions = fetch_newer_versions(registry).await?;
    // BTreeMap keys sort lexicographically, which is wrong for semver.
    let (target_ver, version_info) = match target {
        Some(target) => {
            let info = versions
                .get(target)
                .ok_or_else(|| Error::other(format!("requested version {target} not available")))?;
            (target.to_string(), info)
        }
        None => match versions
            .iter()
            .max_by(|(a, _), (b, _)| crate::system::cmp_versions(a, b))
        {
            Some((ver, info)) => (ver.clone(), info),
            None => return Ok(None),
        },
    };
    let asset = version_info
        .asset_for_platform(&platform)
        .ok_or_else(|| {
            Error::other(format!(
                "no asset for platform '{platform}' in version {target_ver}"
            ))
        })?
        .clone();
    asset.validate(SIG_CONTEXT, asset.all_signers())?;
    Ok(Some((target_ver, asset)))
}

/// Updates to exactly `target` from `registry` with no progress stream; an
/// access point's update, ordered by its Core. Returns only if it failed.
pub async fn install(registry: &str, target: &str) -> Result<(), Error> {
    let (target_ver, asset) = select(registry, Some(target))
        .await?
        .ok_or_else(|| Error::other(format!("requested version {target} not available")))?;
    let tracker = FullProgressTracker::new();
    let mut download_phase = tracker.add_phase("Downloading firmware".into(), Some(100));
    download_phase.start();
    let mut verify_phase = tracker.add_phase("Verifying integrity".into(), Some(10));
    let mut apply_phase = tracker.add_phase("Applying update".into(), Some(1));
    do_update(
        &reqwest::Client::new(),
        asset,
        &target_ver,
        false,
        &mut download_phase,
        &mut verify_phase,
        &mut apply_phase,
    )
    .await
}

/// Main update handler: fetch, validate, download, verify, sysupgrade.
pub async fn update_system(
    ctx: ServerContext,
    UpdateSystemParams {
        registry,
        target_version,
        update_access_points,
    }: UpdateSystemParams,
) -> Result<UpdateSystemRes, Error> {
    let registry = registry.unwrap_or_else(|| {
        crate::system::read_registry_url()
            .unwrap_or_else(|| crate::system::DEFAULT_REGISTRY_URL.to_string())
    });

    let Some((target_ver, asset)) = select(&registry, target_version.as_deref()).await? else {
        return Ok(UpdateSystemRes {
            target: None,
            progress: None,
        });
    };

    // Create progress tracker
    let tracker = FullProgressTracker::new();
    let mut download_phase = tracker.add_phase("Downloading firmware".into(), Some(100));
    // start() must precede set_units/set_total: those only mutate an
    // already-started Progress::Progress, so on a NotStarted phase they are
    // silent no-ops and start() would then reset total/units back to None.
    download_phase.start();
    download_phase.set_units(Some(ProgressUnits::Bytes));
    download_phase.set_total(asset.commitment.size);
    let mut verify_phase = tracker.add_phase("Verifying integrity".into(), Some(10));
    let mut apply_phase = tracker.add_phase("Applying update".into(), Some(1));

    // Register WebSocket continuation for progress streaming.
    // The WS handler creates its own stream from the tracker clone, so the
    // tracker must outlive both the handler and the background update task.
    let guid = Guid::new();
    let guid_str = guid.to_string();
    let tracker_for_ws = tracker.clone();

    ctx.continuations.add(
        guid.clone(),
        RpcContinuation::ws(
            move |mut ws| async move {
                let stream = tracker_for_ws.stream(Some(Duration::from_millis(300)));
                tokio::pin!(stream);
                while let Some(progress) = stream.next().await {
                    let json = match serde_json::to_string(&progress) {
                        Ok(j) => j,
                        Err(_) => break,
                    };
                    if ws
                        .send(axum::extract::ws::Message::Text(json.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    if progress.overall.is_complete() {
                        break;
                    }
                }
                let _ = ws.send(axum::extract::ws::Message::Close(None)).await;
            },
            Duration::from_secs(600),
        ),
    );

    // Spawn background update task (owns tracker + phase handles)
    let client = reqwest::Client::new();
    let target_ver_for_task = target_ver.clone();
    tokio::spawn(async move {
        let target_ver = target_ver_for_task;
        let result = do_update(
            &client,
            asset,
            &target_ver,
            update_access_points,
            &mut download_phase,
            &mut verify_phase,
            &mut apply_phase,
        )
        .await;

        match result {
            Ok(()) => {
                tracing::info!("Update to {target_ver} initiated successfully");
                tracker.complete();
            }
            Err(e) => {
                tracing::error!("Update to {target_ver} failed: {e}");
                crate::activity::log(
                    "system",
                    "update",
                    false,
                    &format!("Update to v{target_ver} failed"),
                    Some(&e.to_string()),
                );
                tracker.fail();
            }
        }
    });

    Ok(UpdateSystemRes {
        target: Some(target_ver),
        progress: Some(guid_str),
    })
}

// ── Pending-update marker ────────────────────────────────────────────
//
// `do_update` writes this file (the target version) just before invoking
// `sysupgrade`. A successful sysupgrade reboots, leaving the marker for the
// freshly-booted firmware to confirm; a failed sysupgrade returns and the
// marker is cleared. `keep.d/startwrt` lists this path so it survives the
// sysupgrade overlay wipe.
const PENDING_UPDATE_MARKER: &str = "/etc/startwrt/pending-update";

/// The marker's line asking the Core to update its access points after it.
const ACCESS_POINTS_LINE: &str = "access-points";

/// Record the in-flight update target so the next boot can confirm it.
/// Best-effort — a marker failure must not abort the update.
fn write_pending_marker(target_ver: &str, access_points: bool) {
    if let Some(dir) = Path::new(PENDING_UPDATE_MARKER).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let content = if access_points {
        format!("{target_ver}\n{ACCESS_POINTS_LINE}\n")
    } else {
        target_ver.to_string()
    };
    if let Err(e) = std::fs::write(PENDING_UPDATE_MARKER, content) {
        tracing::warn!("failed to write pending-update marker: {e}");
    }
}

/// Remove the pending-update marker (the update did not reach a reboot).
fn clear_pending_marker() {
    let _ = std::fs::remove_file(PENDING_UPDATE_MARKER);
}

/// On boot, confirm the outcome of a pending update in the Activity log.
/// Called once from the daemon's normal-mode startup.
pub fn check_pending_update_marker() {
    let (target, access_points) = match std::fs::read_to_string(PENDING_UPDATE_MARKER) {
        Ok(s) => parse_marker(&s),
        Err(_) => return, // no marker — an ordinary boot
    };
    let current = env!("CARGO_PKG_VERSION");
    if target == current {
        if access_points {
            crate::fleet_update::after_core_update();
        }
        crate::activity::log(
            "system",
            "update",
            true,
            &format!("Updated to v{current}"),
            None,
        );
    } else {
        crate::activity::log(
            "system",
            "update",
            false,
            &format!("Update to v{target} failed: booted v{current}"),
            Some("version mismatch after reboot"),
        );
    }
    clear_pending_marker();
}

/// The target version, and whether the access points follow.
fn parse_marker(content: &str) -> (String, bool) {
    let mut lines = content.lines().map(str::trim);
    let target = lines.next().unwrap_or_default().to_string();
    (target, lines.any(|l| l == ACCESS_POINTS_LINE))
}

/// Execute the actual update: download, verify, sysupgrade.
async fn do_update(
    client: &reqwest::Client,
    asset: Asset,
    target_ver: &str,
    access_points: bool,
    download_phase: &mut crate::progress::PhaseProgressTrackerHandle,
    verify_phase: &mut crate::progress::PhaseProgressTrackerHandle,
    apply_phase: &mut crate::progress::PhaseProgressTrackerHandle,
) -> Result<(), Error> {
    let firmware_path = Path::new("/tmp/firmware.bin");

    // Phase 1: Download
    asset
        .download(client, firmware_path, |bytes_written| {
            download_phase.set_done(bytes_written);
        })
        .await?;
    download_phase.complete();

    // Phase 2: Verify (re-read and check blake3)
    verify_phase.start();
    asset.commitment.verify_file(firmware_path).await?;
    verify_phase.complete();

    // Phase 3: Apply via sysupgrade
    apply_phase.start();
    crate::activity::log(
        "system",
        "update",
        true,
        &format!("Applying update to v{target_ver}"),
        None,
    );
    write_pending_marker(target_ver, access_points);
    let status = Command::new("sysupgrade")
        .arg("/tmp/firmware.bin")
        .status()
        .await;
    // Reached only if sysupgrade did not reboot us — i.e. it failed.
    clear_pending_marker();
    let status = status.map_err(|e| Error::other(format!("spawning sysupgrade: {e}")))?;

    if !status.success() {
        return Err(Error::other(format!(
            "sysupgrade exited with code {}",
            status.code().unwrap_or(-1)
        )));
    }
    apply_phase.complete();

    // Device reboots — this point is likely unreachable
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_carries_the_access_point_choice() {
        assert_eq!(parse_marker("1.3.1"), ("1.3.1".into(), false));
        assert_eq!(parse_marker("1.3.1\n"), ("1.3.1".into(), false));
        assert_eq!(
            parse_marker("1.3.1\naccess-points\n"),
            ("1.3.1".into(), true)
        );
    }
}

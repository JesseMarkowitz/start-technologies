use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clap::Parser;
use gpt::partition_types::LINUX_FS;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::context::RpcContext;
use crate::disk::util::{get_capacity, get_mount_source, get_type, is_startos_pool_guid, pvscan};
use crate::disk::{LIVE_MEDIUM_PATH, OsPartitionInfo};
use crate::os_install::gpt::{
    Device, PARTITION_ALIGNMENT_LBA, fresh_gpt, largest_aligned_free_bytes,
};
use crate::os_install::partition_for;
use crate::os_install::quiesce::{list_partitions, update_partition_table};
use crate::prelude::*;
use crate::util::Invoke;
use crate::util::serde::{WithIoFormat, display_serializable};

pub const DEFAULT_LABEL: &str = "StartOS Backups";
/// ext4 stores its volume label in 16 bytes.
const MAX_LABEL_BYTES: usize = 16;

#[derive(Deserialize, Serialize, Parser, VisitFields)]
#[group(skip)]
#[serde(rename_all = "camelCase")]
#[command(rename_all = "kebab-case")]
pub struct FormatDiskParams {
    #[arg(help = "help.arg.format-disk")]
    pub disk: PathBuf,
    #[arg(long, help = "help.arg.format-label")]
    pub label: Option<String>,
}

rpc_toolkit::reflect_ts!(FormatDiskParams);

/// Erases a whole drive and leaves it with a single ext4 partition, returning
/// that partition's device node. Refuses the drive carrying the running system,
/// a StartOS data pool, the live boot medium, or anything mounted or stacked on.
pub async fn format(
    ctx: RpcContext,
    FormatDiskParams { disk, label }: FormatDiskParams,
) -> Result<PathBuf, Error> {
    let label = label.unwrap_or_else(|| DEFAULT_LABEL.to_owned());
    if label.len() > MAX_LABEL_BYTES {
        return Err(refused(t!(
            "disk.format.label-too-long",
            max = MAX_LABEL_BYTES
        )));
    }
    let disk = whole_drive(&disk).await?;
    refuse_protected(&ctx.os_partitions, &disk).await?;
    let capacity = get_capacity(&disk).await?;

    Command::new("wipefs")
        .arg("-a")
        .arg(&disk)
        .invoke(ErrorKind::DiskManagement)
        .await?;
    let gpt_disk = disk.clone();
    let name = label.clone();
    tokio::task::spawn_blocking(move || {
        let device: Device = Box::new(
            std::fs::File::options()
                .read(true)
                .write(true)
                .open(&gpt_disk)?,
        );
        write_single_partition(device, capacity, &name)
    })
    .await
    .unwrap()?;
    update_partition_table(&disk).await?;

    let partition = partition_for(&disk, 1);
    Command::new("wipefs")
        .arg("-a")
        .arg(&partition)
        .invoke(ErrorKind::DiskManagement)
        .await?;
    Command::new("mkfs.ext4")
        .arg("-q")
        .arg("-F")
        .arg("-L")
        .arg(&label)
        .arg(&partition)
        .invoke(ErrorKind::DiskManagement)
        .await?;
    Command::new("udevadm")
        .arg("settle")
        .invoke(ErrorKind::DiskManagement)
        .await?;
    Ok(partition)
}

pub fn display_formatted(
    params: WithIoFormat<FormatDiskParams>,
    partition: PathBuf,
) -> Result<(), Error> {
    if let Some(format) = params.format {
        return display_serializable(format, partition);
    }
    println!(
        "{}",
        t!("disk.format.formatted", partition = partition.display())
    );
    Ok(())
}

/// A fresh GPT holding one Linux filesystem partition over the whole device.
pub(crate) fn write_single_partition(
    device: Device,
    capacity: u64,
    name: &str,
) -> Result<(), Error> {
    let mut gpt = fresh_gpt(device, capacity)?;
    let size = largest_aligned_free_bytes(&gpt)?;
    gpt.add_partition(name, size, LINUX_FS, 0, Some(PARTITION_ALIGNMENT_LBA))?;
    gpt.write()?;
    Ok(())
}

fn refused(message: impl std::fmt::Display) -> Error {
    Error::new(eyre!("{message}"), ErrorKind::InvalidRequest)
}

/// Canonical node of a whole drive: what `lsblk` types as `disk`, which rules
/// out partitions, optical drives, and virtual devices (loop, device-mapper).
async fn whole_drive(path: &Path) -> Result<PathBuf, Error> {
    let disk = tokio::fs::canonicalize(path)
        .await
        .with_ctx(|_| (ErrorKind::BlockDevice, path.display().to_string()))?;
    if get_type(&disk).await? != "disk" {
        return Err(Error::new(
            eyre!("{}", t!("disk.format.not-a-drive", disk = path.display())),
            ErrorKind::BlockDevice,
        ));
    }
    Ok(disk)
}

async fn canonical(path: &Path) -> PathBuf {
    tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| path.to_owned())
}

async fn refuse_protected(os: &OsPartitionInfo, disk: &Path) -> Result<(), Error> {
    let mut devices = vec![disk.to_owned()];
    devices.extend(list_partitions(disk).await?);

    if devices
        .iter()
        .any(|d| os.contains(d) || os.data.as_deref() == Some(d))
    {
        return Err(refused(t!(
            "disk.format.system-drive",
            disk = disk.display()
        )));
    }
    for (pv, vg) in pvscan().await? {
        if vg.as_deref().is_some_and(is_startos_pool_guid)
            && devices.contains(&canonical(&pv).await)
        {
            return Err(refused(t!(
                "disk.format.startos-data",
                disk = disk.display()
            )));
        }
    }
    if let Ok(Some(live)) = get_mount_source(LIVE_MEDIUM_PATH).await {
        if devices.contains(&live) {
            return Err(refused(t!(
                "disk.format.live-medium",
                disk = disk.display()
            )));
        }
    }
    if let Some(by) = in_use(&devices).await? {
        return Err(refused(t!(
            "disk.format.in-use",
            disk = disk.display(),
            by = by
        )));
    }
    Ok(())
}

/// What holds one of `devices`: a mountpoint, swap, or a device stacked on it.
async fn in_use(devices: &[PathBuf]) -> Result<Option<String>, Error> {
    let devices: BTreeSet<&Path> = devices.iter().map(PathBuf::as_path).collect();
    for (table, usage) in [("/proc/mounts", None), ("/proc/swaps", Some("swap"))] {
        let content = tokio::fs::read_to_string(table)
            .await
            .with_ctx(|_| (ErrorKind::Filesystem, table))?;
        for line in content.lines() {
            let mut fields = line.split_whitespace();
            let Some(source) = fields.next().filter(|s| s.starts_with('/')) else {
                continue;
            };
            if devices.contains(canonical(Path::new(source)).await.as_path()) {
                return Ok(Some(
                    usage.or_else(|| fields.next()).unwrap_or(table).to_owned(),
                ));
            }
        }
    }
    for dev in devices {
        let Some(name) = dev.file_name() else {
            continue;
        };
        let holders = Path::new("/sys/class/block").join(name).join("holders");
        let Ok(mut entries) = tokio::fs::read_dir(&holders).await else {
            continue;
        };
        if let Some(holder) = entries.next_entry().await? {
            return Ok(Some(
                Path::new("/dev")
                    .join(holder.file_name())
                    .display()
                    .to_string(),
            ));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use gpt::GptConfig;
    use gpt::disk::LogicalBlockSize;

    use super::*;

    #[test]
    fn single_partition_spans_the_disk() {
        for size_gb in [8u64, 64, 2048] {
            let path = std::env::temp_dir().join(format!(
                "startos-format-test-{}-{}.img",
                std::process::id(),
                size_gb
            ));
            let capacity = size_gb * 1024 * 1024 * 1024;
            std::fs::File::create(&path)
                .unwrap()
                .set_len(capacity)
                .unwrap();

            let device: Device = Box::new(
                std::fs::File::options()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap(),
            );
            write_single_partition(device, capacity, DEFAULT_LABEL).unwrap();

            let dev = Box::new(std::fs::File::options().read(true).open(&path).unwrap());
            let gpt = GptConfig::new()
                .writable(false)
                .logical_block_size(LogicalBlockSize::Lb512)
                .open_from_device(dev)
                .unwrap();
            let parts = gpt.partitions();
            assert_eq!(parts.len(), 1, "{size_gb} GiB");
            let part = parts.get(&1).unwrap();
            assert_eq!(part.name, DEFAULT_LABEL);
            assert_eq!(part.part_type_guid, LINUX_FS);
            assert_eq!(part.first_lba, PARTITION_ALIGNMENT_LBA, "{size_gb} GiB");
            assert_eq!(part.last_lba, capacity / 512 - 34, "{size_gb} GiB");

            std::fs::remove_file(&path).ok();
        }
    }
}

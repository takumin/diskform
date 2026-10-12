//! Applying (ADR 0004, ADR 0018): performs the operations of a plan in
//! order. Before each operation it checks that the devices it writes on
//! are blank, and after it, that what it made is as planned. Reading goes
//! through `System` and changing through `Create`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::change::Create;
use crate::destroy::signatures;
use crate::layout::{EXTENT_SIZE, PV_DATA_OFFSET, format_size};
use crate::model::{Declaration, Format, PartitionType, Ref};
use crate::plan::{Operation, Plan};
use crate::system::System;

/// Performs the operations of `plan`, calling `done` after each. Stops at
/// the first failure, leaving what was done as it is (ADR 0018).
pub fn execute(
    decl: &Declaration,
    plan: &Plan,
    sys: &dyn System,
    create: &dyn Create,
    done: &mut dyn FnMut(&Operation),
) -> Result<(), String> {
    let a = Applier {
        decl,
        plan,
        sys,
        create,
    };
    for op in &plan.operations {
        a.perform(op).map_err(|e| format!("{op}: {e}"))?;
        done(op);
    }
    Ok(())
}

struct Applier<'a> {
    decl: &'a Declaration,
    plan: &'a Plan,
    sys: &'a dyn System,
    create: &'a dyn Create,
}

impl Applier<'_> {
    fn perform(&self, op: &Operation) -> Result<(), String> {
        let sys = self.sys;
        match op {
            Operation::CreateTable { disk, table, .. } => {
                let disk = &self.plan.disks[disk];
                let state = sys.disk_state(disk)?;
                if !state.is_empty() {
                    return Err(format!(
                        "{} is no longer empty: {}",
                        disk.path.display(),
                        crate::destroy::describe(&state)
                    ));
                }
                self.create.create_table(&disk.path, *table)?;
                match sys.partition_table(disk)? {
                    Some(t) if t.kind == table.to_string() && t.partitions.is_empty() => Ok(()),
                    _ => Err(format!(
                        "{} has no empty {table} partition table",
                        disk.path.display()
                    )),
                }
            }
            Operation::CreatePartition {
                disk,
                number,
                extent,
                kind,
                label,
                ..
            } => {
                let disk = &self.plan.disks[disk];
                let path = disk.path.display();
                let count = sys.partition_table(disk)?.map_or(0, |t| t.partitions.len());
                if count + 1 != *number {
                    return Err(format!(
                        "{path} has {count} partitions instead of {}",
                        number - 1
                    ));
                }
                let guid = PartitionType::gpt_guid(*kind);
                let sector = disk.logical_sector;
                self.create.create_partition(
                    &disk.path,
                    *number,
                    extent.start / sector,
                    extent.size / sector,
                    guid,
                    label.as_str(),
                )?;
                let table = sys.partition_table(disk)?;
                let entry = table
                    .iter()
                    .flat_map(|t| &t.partitions)
                    .find(|e| e.number as usize == *number)
                    .ok_or_else(|| format!("{path} has no partition {number}"))?;
                if entry.start != extent.start
                    || entry.size != extent.size
                    || entry.type_guid != guid
                    || entry.name != label.as_str()
                {
                    return Err(format!(
                        "partition {number} on {path} is {} at {}, type {}, name {:?}",
                        format_size(entry.size),
                        format_size(entry.start),
                        entry.type_guid,
                        entry.name
                    ));
                }
                Ok(())
            }
            Operation::CreateLuks { name, device } => {
                let keyfile = self.decl.luks[name].keyfile.as_str();
                // ADR 0009: the key file is checked again right before use.
                sys.check_keyfile(Path::new(keyfile))
                    .map_err(|e| format!("`{keyfile}` {e}"))?;
                let keyfile = Path::new(keyfile);
                let lower = self.device(device)?;
                self.blank(&lower)?;
                self.create.format_luks(&lower, keyfile)?;
                let probe = sys.probe(&lower)?;
                if probe.kind.as_deref() != Some("crypto_LUKS")
                    || probe.version.as_deref() != Some("2")
                {
                    return Err(format!("{} has no LUKS2 header", lower.display()));
                }
                self.create.open_luks(&lower, keyfile, name.as_str())?;
                let mapping = sys
                    .luks_mapping(&lower)?
                    .ok_or_else(|| format!("{} is not open", lower.display()))?;
                // ADR 0009: the data offset decides the size of the mapping.
                let size = sys
                    .block_device(&mapping)
                    .map_err(|e| format!("cannot read {}: {e}", mapping.display()))?
                    .map_or(0, |b| b.size);
                let planned = self.plan.layout.luks[name];
                if size != planned {
                    return Err(format!(
                        "{} is {}, but the plan computed {}",
                        mapping.display(),
                        format_size(size),
                        format_size(planned)
                    ));
                }
                Ok(())
            }
            Operation::CreatePhysicalVolume { device } => {
                let device = self.device(device)?;
                self.blank(&device)?;
                self.create.create_physical_volume(&device)?;
                let shown = device.display();
                if sys.probe(&device)?.kind.as_deref() != Some("LVM2_member") {
                    return Err(format!("{shown} has no physical volume label"));
                }
                // ADR 0010.
                match sys.pv_data_offset(&device)? {
                    Some(PV_DATA_OFFSET) => Ok(()),
                    Some(offset) => Err(format!(
                        "the extents of {shown} start at {}, not at {}",
                        format_size(offset),
                        format_size(PV_DATA_OFFSET)
                    )),
                    None => Err(format!("LVM does not find a physical volume on {shown}")),
                }
            }
            Operation::CreateVolumeGroup { name, devices } => {
                let devices = devices
                    .iter()
                    .map(|d| self.device(d))
                    .collect::<Result<Vec<_>, _>>()?;
                self.create.create_volume_group(name.as_str(), &devices)?;
                let groups = sys.volume_groups()?;
                let group = groups
                    .get(name.as_str())
                    .ok_or_else(|| format!("no volume group named `{name}` exists"))?;
                let devices: BTreeSet<PathBuf> = devices.into_iter().collect();
                if group.extent_size != EXTENT_SIZE || group.devices != devices {
                    return Err(format!(
                        "the volume group has the extent size {} and the physical volumes {:?}",
                        format_size(group.extent_size),
                        group.devices
                    ));
                }
                Ok(())
            }
            Operation::CreateLogicalVolume { vg, name, size } => {
                self.create
                    .create_logical_volume(vg.as_str(), name.as_str(), *size)?;
                let groups = sys.volume_groups()?;
                let volume = groups
                    .get(vg.as_str())
                    .and_then(|g| g.volumes.get(name.as_str()))
                    .ok_or_else(|| format!("no logical volume named `{name}` exists"))?;
                if volume.size != *size {
                    return Err(format!(
                        "the logical volume is {}",
                        format_size(volume.size)
                    ));
                }
                if volume.device.is_none() {
                    return Err("the logical volume is not active".to_owned());
                }
                Ok(())
            }
            Operation::CreateFilesystem {
                format,
                devices,
                label,
                profiles,
                ..
            } => {
                let paths = devices
                    .iter()
                    .map(|d| self.device(d))
                    .collect::<Result<Vec<_>, _>>()?;
                for path in &paths {
                    self.blank(path)?;
                }
                self.create
                    .create_filesystem(*format, &paths, label.as_str(), *profiles)?;
                let mut uuids = BTreeSet::new();
                for path in &paths {
                    let probe = sys.probe(path)?;
                    let found = probe.label.as_deref().unwrap_or("");
                    if probe.kind.as_deref() != Some(&format.to_string()) || found != label.as_str()
                    {
                        return Err(format!(
                            "{} has {}, label {found:?}",
                            path.display(),
                            signatures(&probe).join(", ")
                        ));
                    }
                    uuids.insert(probe.uuid);
                }
                if *format != Format::Btrfs {
                    return Ok(());
                }
                let [Some(uuid)] = <[_; 1]>::try_from(uuids.into_iter().collect::<Vec<_>>())
                    .map_err(|_| "the devices do not have one btrfs UUID".to_owned())?
                else {
                    return Err("blkid found no btrfs UUID".to_owned());
                };
                let state = self.mounted(&paths, |_| {
                    sys.btrfs(&uuid)?
                        .ok_or_else(|| format!("btrfs {uuid} is not mounted"))
                })?;
                let expected: BTreeSet<PathBuf> = paths.into_iter().collect();
                if state.devices != expected {
                    return Err(format!("btrfs {uuid} has the devices {:?}", state.devices));
                }
                if let Some((data, metadata)) = profiles {
                    let only = |p: String| BTreeSet::from([p]);
                    if state.data_profiles != only(data.to_string())
                        || state.metadata_profiles != only(metadata.to_string())
                    {
                        return Err(format!(
                            "btrfs {uuid} stores data as {:?} and metadata as {:?}",
                            state.data_profiles, state.metadata_profiles
                        ));
                    }
                }
                Ok(())
            }
            Operation::CreateSubvolume { filesystem, path } => {
                let fs = &self.decl.filesystem[filesystem];
                let paths = fs
                    .device
                    .iter()
                    .chain(fs.devices.iter().flatten())
                    .map(|d| self.device(d))
                    .collect::<Result<Vec<_>, _>>()?;
                let uuid = sys
                    .probe(&paths[0])?
                    .uuid
                    .ok_or("blkid found no btrfs UUID")?;
                self.mounted(&paths, |dir| {
                    self.create.create_subvolume(dir, path.as_str())?;
                    let state = sys
                        .btrfs(&uuid)?
                        .ok_or_else(|| format!("btrfs {uuid} is not mounted"))?;
                    if !state.subvolumes.contains(path.as_str()) {
                        return Err(format!("btrfs {uuid} has no subvolume `{}`", path.as_str()));
                    }
                    Ok(())
                })
            }
            Operation::CreateSwap { device, label, .. } => {
                let device = self.device(device)?;
                self.blank(&device)?;
                self.create.create_swap(&device, label.as_str())?;
                let probe = sys.probe(&device)?;
                let found = probe.label.as_deref().unwrap_or("");
                if probe.kind.as_deref() != Some("swap") || found != label.as_str() {
                    return Err(format!(
                        "{} has {}, label {found:?}",
                        device.display(),
                        signatures(&probe).join(", ")
                    ));
                }
                Ok(())
            }
        }
    }

    /// ADR 0018: apply writes only on devices where blkid finds nothing.
    /// New partitions and logical volumes may show what earlier storage
    /// left at the same place.
    fn blank(&self, device: &Path) -> Result<(), String> {
        let probe = self.sys.probe(device)?;
        let found = signatures(&probe);
        if found.is_empty() {
            return Ok(());
        }
        Err(format!(
            "{} has {}, which earlier storage left there; apply does not write over it \
             (ADR 0018)",
            device.display(),
            found.join(", ")
        ))
    }

    /// The device that is a declared one now.
    fn device(&self, r: &Ref) -> Result<PathBuf, String> {
        match r {
            Ref::Partition { disk, partition } => {
                let number = self.decl.disk[disk]
                    .partitions
                    .iter()
                    .position(|p| &p.name == partition)
                    .expect("a valid declaration")
                    + 1;
                let table = self.sys.partition_table(&self.plan.disks[disk])?;
                table
                    .into_iter()
                    .flat_map(|t| t.partitions)
                    .find(|e| e.number as usize == number)
                    .map(|e| e.device)
                    .ok_or_else(|| format!("{r} does not exist"))
            }
            Ref::Luks { name } => {
                let lower = self.device(&self.decl.luks[name].device)?;
                self.sys
                    .luks_mapping(&lower)?
                    .ok_or_else(|| format!("{r} is not open"))
            }
            Ref::LogicalVolume { vg, lv } => self
                .sys
                .volume_groups()?
                .get(vg.as_str())
                .and_then(|g| g.volumes.get(lv.as_str()))
                .and_then(|v| v.device.clone())
                .ok_or_else(|| format!("{r} is not active")),
        }
    }

    /// Runs `f` while the btrfs on `devices` is mounted on a directory of
    /// its own, and unmounts it afterwards.
    fn mounted<T>(
        &self,
        devices: &[PathBuf],
        f: impl FnOnce(&Path) -> Result<T, String>,
    ) -> Result<T, String> {
        let dir = self.create.mount_btrfs(devices)?;
        let result = f(&dir);
        match self.create.unmount(&dir) {
            Ok(()) => result,
            Err(e) => {
                let left = format!("{e}; the btrfs is left mounted at {}", dir.display());
                Err(match result {
                    Ok(_) => left,
                    Err(first) => format!("{first}; then {left}"),
                })
            }
        }
    }
}

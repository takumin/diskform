//! Judging whether the existing storage of a group of disks is configured
//! as declared (ADR 0011, ADR 0015). It reads the machine through `System`
//! and never changes a device.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::layout::{EXTENT_SIZE, Layout, PARTITION_ALIGNMENT, format_size};
use crate::model::{Declaration, Filesystem, Format, Label, Name, PartitionType, Ref};
use crate::plan::{disks_of, unused};
use crate::system::{BlockDevice, DiskState, Probe, System, VolumeGroupState};
use crate::validate::Issue;

/// The disks of a group, with what plan has already read about them.
pub struct GroupDisk<'a> {
    pub name: &'a Name,
    pub device: &'a BlockDevice,
    pub state: &'a DiskState,
}

/// The differences between the existing storage of a group and the
/// declaration, and the items that cannot be compared without changing the
/// state of the machine. The group is configured if there are none.
pub fn judge(
    decl: &Declaration,
    sys: &dyn System,
    layout: &Layout,
    group: &[GroupDisk],
) -> Vec<Issue> {
    let mut j = Judge {
        decl,
        sys,
        layout,
        issues: Vec::new(),
        devices: BTreeMap::new(),
        judged_vgs: BTreeSet::new(),
        existing_vgs: None,
    };
    for disk in group {
        j.disk(disk);
    }
    let names: BTreeSet<&Name> = group.iter().map(|d| d.name).collect();
    // A group is closed under references, so an element is in the group
    // if any of its devices is.
    let in_group = |r: &Ref| disks_of(decl, r).iter().any(|d| names.contains(d));
    for (name, luks) in &decl.luks {
        if in_group(&luks.device) {
            j.device(&Ref::Luks { name: name.clone() });
        }
    }
    for (name, vg) in &decl.lvm {
        if vg.devices.iter().any(in_group) {
            j.volume_group(name);
        }
    }
    for (name, fs) in &decl.filesystem {
        let devices: Vec<Ref> = fs
            .device
            .iter()
            .chain(fs.devices.iter().flatten())
            .cloned()
            .collect();
        if devices.iter().any(in_group) {
            j.filesystem(name, fs, &devices);
        }
    }
    for (name, swap) in &decl.swap {
        if in_group(&swap.device) {
            j.swap(name, &swap.device, &swap.label);
        }
    }
    j.issues
}

fn describe_label(label: Option<&str>) -> String {
    match label {
        None | Some("") => "no label".to_owned(),
        Some(l) => format!("label {l:?}"),
    }
}

fn label_matches(declared: &Label, found: Option<&str>) -> bool {
    found.unwrap_or("") == declared.as_str()
}

fn describe_signature(p: &Probe) -> String {
    match (&p.kind, &p.version) {
        (None, _) => "no signature".to_owned(),
        (Some(k), Some(v)) if k == "crypto_LUKS" => format!("a LUKS{v} header"),
        (Some(k), _) => format!("a {k} signature"),
    }
}

fn describe_paths(paths: &BTreeSet<PathBuf>) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

struct Judge<'a> {
    decl: &'a Declaration,
    sys: &'a dyn System,
    layout: &'a Layout,
    issues: Vec<Issue>,
    /// The existing devices of the declared ones that could be found.
    devices: BTreeMap<Ref, Option<PathBuf>>,
    judged_vgs: BTreeSet<Name>,
    /// Read once; `None` inside if reading failed.
    existing_vgs: Option<Option<BTreeMap<String, VolumeGroupState>>>,
}

impl Judge<'_> {
    fn report(&mut self, at: String, message: String) {
        self.issues.push(Issue { at, message });
    }

    fn probe(&mut self, at: &str, device: &Path) -> Option<Probe> {
        match self.sys.probe(device) {
            Ok(p) => Some(p),
            Err(e) => {
                self.report(at.to_owned(), e);
                None
            }
        }
    }

    fn disk(&mut self, disk: &GroupDisk) {
        let name = disk.name;
        let declared = &self.decl.disk[name];
        let path = disk.device.path.display();
        let at = format!("disk.{name}");
        if !disk.state.holders.is_empty() {
            self.report(
                at.clone(),
                format!("{path} itself is used by {}", disk.state.holders.join(", ")),
            );
        }
        let Some(probe) = self.probe(&at, &disk.device.path) else {
            return;
        };
        if probe.kind.is_some() {
            self.report(
                at.clone(),
                format!("{path} itself has {}", describe_signature(&probe)),
            );
        }
        let table = match self.sys.partition_table(disk.device) {
            Ok(Some(t)) => t,
            Ok(None) => {
                self.report(
                    at,
                    format!(
                        "{path} has no partition table, but the declaration needs {}",
                        declared.table
                    ),
                );
                return;
            }
            Err(e) => {
                self.report(at, e);
                return;
            }
        };
        if table.kind != declared.table.to_string() {
            self.report(
                format!("{at}.table"),
                format!(
                    "{path} has a {} partition table, but the declaration needs {}",
                    table.kind, declared.table
                ),
            );
            return;
        }

        let numbers: Vec<u32> = table.partitions.iter().map(|p| p.number).collect();
        let expected: Vec<u32> = (1..).take(declared.partitions.len()).collect();
        if numbers != expected {
            self.report(
                format!("{at}.partitions"),
                format!(
                    "{path} has partitions {numbers:?}, but the declaration needs {expected:?}"
                ),
            );
        }
        let extents = self.layout.partitions.get(name);
        for (i, p) in declared.partitions.iter().enumerate() {
            let number = i as u32 + 1;
            let Some(entry) = table.partitions.iter().find(|e| e.number == number) else {
                continue;
            };
            let at = format!("{at}.partitions[{i}]");
            let guid = PartitionType::gpt_guid(p.kind);
            if entry.type_guid != guid {
                self.report(
                    at.clone(),
                    format!(
                        "partition {number} has type {}, but the declaration needs {} ({guid})",
                        entry.type_guid,
                        p.kind.map_or("linux".to_owned(), |k| k.to_string()),
                    ),
                );
            }
            if let Some(x) = extents.map(|e| e[i]) {
                if entry.start.abs_diff(x.start) > PARTITION_ALIGNMENT
                    || entry.size.abs_diff(x.size) > PARTITION_ALIGNMENT
                {
                    self.report(
                        at.clone(),
                        format!(
                            "partition {number} is {} at {}, but the declaration needs {} at {}",
                            format_size(entry.size),
                            format_size(entry.start),
                            format_size(x.size),
                            format_size(x.start)
                        ),
                    );
                }
            }
            if !label_matches(&p.label, Some(&entry.name)) {
                self.report(
                    format!("{at}.label"),
                    format!(
                        "partition {number} has {}, but the declaration needs {}",
                        describe_label(Some(&entry.name)),
                        describe_label(Some(p.label.as_str()))
                    ),
                );
            }
            let r = Ref::Partition {
                disk: name.clone(),
                partition: p.name.clone(),
            };
            self.devices.insert(r, Some(entry.device.clone()));
        }
    }

    /// The existing device of a declared one; `None` if it was not found
    /// or cannot be read, which has already been reported.
    fn device(&mut self, r: &Ref) -> Option<PathBuf> {
        if let Some(d) = self.devices.get(r) {
            return d.clone();
        }
        let d = match r {
            // A partition that was not found has been reported with its disk.
            Ref::Partition { .. } => None,
            Ref::Luks { name } => self.luks(name),
            Ref::LogicalVolume { vg, .. } => {
                self.volume_group(vg);
                return self.devices.get(r).cloned().flatten();
            }
        };
        self.devices.insert(r.clone(), d.clone());
        d
    }

    fn luks(&mut self, name: &Name) -> Option<PathBuf> {
        let luks = &self.decl.luks[name];
        let device = self.device(&luks.device)?;
        let at = format!("luks.{name}");
        let shown = format!("{} ({})", luks.device, device.display());
        let probe = self.probe(&at, &device)?;
        if probe.kind.as_deref() != Some("crypto_LUKS") || probe.version.as_deref() != Some("2") {
            self.report(
                at,
                format!(
                    "{shown} has {}, but the declaration needs a LUKS2 header",
                    describe_signature(&probe)
                ),
            );
            return None;
        }
        let keyfile = Path::new(luks.keyfile.as_str());
        // A key file that cannot be used has been reported by plan.
        if self.sys.check_keyfile(keyfile).is_ok() {
            match self.sys.test_keyfile(&device, keyfile) {
                Ok(true) => {}
                Ok(false) => self.report(
                    format!("{at}.keyfile"),
                    format!("`{}` opens no keyslot of {shown}", luks.keyfile.as_str()),
                ),
                Err(e) => self.report(format!("{at}.keyfile"), e),
            }
        }
        match self.sys.luks_mapping(&device) {
            Ok(Some(mapping)) => Some(mapping),
            Ok(None) => {
                // Nothing is left uncompared if nothing is built on it.
                if !unused(self.decl).contains(&Ref::Luks { name: name.clone() }) {
                    self.report(
                        at,
                        format!(
                            "{shown} is not open, so what is built on it cannot be compared \
                             without opening it (ADR 0011)"
                        ),
                    );
                }
                None
            }
            Err(e) => {
                self.report(at, e);
                None
            }
        }
    }

    fn existing_vg(&mut self, name: &Name) -> Result<Option<VolumeGroupState>, ()> {
        if self.existing_vgs.is_none() {
            let vgs = match self.sys.volume_groups() {
                Ok(vgs) => Some(vgs),
                Err(e) => {
                    self.report("lvm".to_owned(), e);
                    None
                }
            };
            self.existing_vgs = Some(vgs);
        }
        match self.existing_vgs.as_ref().and_then(Option::as_ref) {
            Some(vgs) => Ok(vgs.get(name.as_str()).cloned()),
            None => Err(()),
        }
    }

    fn volume_group(&mut self, name: &Name) {
        if !self.judged_vgs.insert(name.clone()) {
            return;
        }
        let vg = &self.decl.lvm[name];
        let Some(devices) = vg
            .devices
            .iter()
            .map(|d| self.device(d))
            .collect::<Option<BTreeSet<PathBuf>>>()
        else {
            return;
        };
        let at = format!("lvm.{name}");
        let Ok(existing) = self.existing_vg(name) else {
            return;
        };
        let Some(existing) = existing else {
            self.report(at, format!("no volume group named `{name}` exists"));
            return;
        };
        if existing.extent_size != EXTENT_SIZE {
            self.report(
                at.clone(),
                format!(
                    "the physical extent size is {}, but must be {} (ADR 0010)",
                    format_size(existing.extent_size),
                    format_size(EXTENT_SIZE)
                ),
            );
        }
        if existing.devices != devices {
            self.report(
                format!("{at}.devices"),
                format!(
                    "the physical volumes are {}, but the declaration needs {}",
                    describe_paths(&existing.devices),
                    describe_paths(&devices)
                ),
            );
        }
        let extra: Vec<&str> = existing
            .volumes
            .keys()
            .map(String::as_str)
            .filter(|lv| !vg.volumes.keys().any(|d| d.as_str() == *lv))
            .collect();
        if !extra.is_empty() {
            self.report(
                format!("{at}.volumes"),
                format!(
                    "logical volumes that are not declared exist: {}",
                    extra.join(", ")
                ),
            );
        }
        let sizes = self.layout.volume_groups.get(name).map(|g| &g.volumes);
        for lv in vg.volumes.keys() {
            let at = format!("{at}.volumes.{lv}");
            let Some(found) = existing.volumes.get(lv.as_str()) else {
                self.report(at, format!("no logical volume named `{lv}` exists"));
                continue;
            };
            if let Some(size) = sizes.and_then(|s| s.get(lv)) {
                if found.size.abs_diff(*size) > EXTENT_SIZE {
                    self.report(
                        at.clone(),
                        format!(
                            "the logical volume is {}, but the declaration needs {}",
                            format_size(found.size),
                            format_size(*size)
                        ),
                    );
                }
            }
            if found.device.is_none() {
                self.report(
                    at,
                    "the logical volume is not active, so what is on it cannot be compared \
                     without activating it (ADR 0015)"
                        .to_owned(),
                );
            }
            let r = Ref::LogicalVolume {
                vg: name.clone(),
                lv: lv.clone(),
            };
            self.devices.insert(r, found.device.clone());
        }
    }

    fn filesystem(&mut self, name: &Name, fs: &Filesystem, devices: &[Ref]) {
        let Some(paths) = devices
            .iter()
            .map(|d| self.device(d))
            .collect::<Option<Vec<PathBuf>>>()
        else {
            return;
        };
        let at = format!("filesystem.{name}");
        let format = fs.format.to_string();
        let mut uuids = BTreeSet::new();
        let mut label_reported = false;
        for (r, path) in devices.iter().zip(&paths) {
            let Some(probe) = self.probe(&at, path) else {
                return;
            };
            let shown = format!("{r} ({})", path.display());
            if probe.kind.as_deref() != Some(format.as_str()) {
                self.report(
                    format!("{at}.format"),
                    format!(
                        "{shown} has {}, but the declaration needs {format}",
                        describe_signature(&probe)
                    ),
                );
                return;
            }
            // Every device of a btrfs has the same label.
            if !label_reported && !label_matches(&fs.label, probe.label.as_deref()) {
                label_reported = true;
                self.report(
                    format!("{at}.label"),
                    format!(
                        "{shown} has {}, but the declaration needs {}",
                        describe_label(probe.label.as_deref()),
                        describe_label(Some(fs.label.as_str()))
                    ),
                );
            }
            uuids.insert(probe.uuid);
        }
        if fs.format == Format::Btrfs {
            self.btrfs(&at, fs, &paths, uuids);
        }
    }

    fn btrfs(
        &mut self,
        at: &str,
        fs: &Filesystem,
        paths: &[PathBuf],
        uuids: BTreeSet<Option<String>>,
    ) {
        let uuid = match <[_; 1]>::try_from(uuids.into_iter().collect::<Vec<_>>()) {
            Ok([Some(uuid)]) => uuid,
            Ok([None]) => {
                self.report(at.to_owned(), "blkid found no btrfs UUID".to_owned());
                return;
            }
            Err(_) => {
                self.report(
                    format!("{at}.devices"),
                    "the devices belong to different btrfs filesystems".to_owned(),
                );
                return;
            }
        };
        let existing = match self.sys.btrfs(&uuid) {
            Ok(Some(b)) => b,
            Ok(None) => {
                self.report(
                    at.to_owned(),
                    format!(
                        "btrfs {uuid} is not mounted, so its devices, profiles and subvolumes \
                         cannot be compared without mounting it (ADR 0015)"
                    ),
                );
                return;
            }
            Err(e) => {
                self.report(at.to_owned(), e);
                return;
            }
        };
        let paths: BTreeSet<PathBuf> = paths.iter().cloned().collect();
        if existing.devices != paths {
            self.report(
                format!("{at}.devices"),
                format!(
                    "btrfs {uuid} has the devices {}, but the declaration needs {}",
                    describe_paths(&existing.devices),
                    describe_paths(&paths)
                ),
            );
        }
        if let Some(options) = &fs.btrfs {
            let checks = [
                (
                    "data_profile",
                    "data",
                    options.data_profile,
                    &existing.data_profiles,
                ),
                (
                    "metadata_profile",
                    "metadata",
                    options.metadata_profile,
                    &existing.metadata_profiles,
                ),
            ];
            for (key, what, declared, found) in checks {
                let declared = declared.to_string();
                if found.len() != 1 || !found.contains(&declared) {
                    let found: Vec<&str> = found.iter().map(String::as_str).collect();
                    self.report(
                        format!("{at}.btrfs.{key}"),
                        format!(
                            "btrfs {uuid} stores {what} as {}, but the declaration needs {declared}",
                            found.join(" and ")
                        ),
                    );
                }
            }
        }
        for (sub, s) in fs.subvolumes.iter().flatten() {
            if !existing.subvolumes.contains(s.path.as_str()) {
                self.report(
                    format!("{at}.subvolumes.{sub}"),
                    format!("btrfs {uuid} has no subvolume `{}`", s.path.as_str()),
                );
            }
        }
    }

    fn swap(&mut self, name: &Name, device: &Ref, label: &Label) {
        let Some(path) = self.device(device) else {
            return;
        };
        let at = format!("swap.{name}");
        let Some(probe) = self.probe(&at, &path) else {
            return;
        };
        let shown = format!("{device} ({})", path.display());
        if probe.kind.as_deref() != Some("swap") {
            self.report(
                at,
                format!(
                    "{shown} has {}, but the declaration needs swap",
                    describe_signature(&probe)
                ),
            );
        } else if !label_matches(label, probe.label.as_deref()) {
            self.report(
                format!("{at}.label"),
                format!(
                    "{shown} has {}, but the declaration needs {}",
                    describe_label(probe.label.as_deref()),
                    describe_label(Some(label.as_str()))
                ),
            );
        }
    }
}

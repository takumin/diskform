//! Planning (ADR 0004): resolves the disks of a valid declaration, computes
//! sizes, judges each group of connected disks, and lists the operations
//! that apply would perform. It reads the machine through `System` and
//! never changes a device.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::judge::{self, GroupDisk};
use crate::layout::{self, Extent, Geometry, Layout, format_size};
use crate::model::{
    BtrfsProfile, Declaration, Format, Label, Match, Name, PartitionType, Ref, SubvolumePath, Table,
};
use crate::system::{BlockDevice, DeviceKind, DiskState, System};
use crate::validate::Issue;

/// One step of apply, in the order apply would perform it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    CreateTable {
        disk: Name,
        device: PathBuf,
        table: Table,
    },
    CreatePartition {
        disk: Name,
        number: usize,
        name: Name,
        extent: Extent,
        kind: Option<PartitionType>,
        label: Label,
    },
    CreateLuks {
        name: Name,
        device: Ref,
    },
    CreatePhysicalVolume {
        device: Ref,
    },
    CreateVolumeGroup {
        name: Name,
        devices: Vec<Ref>,
    },
    CreateLogicalVolume {
        vg: Name,
        name: Name,
        size: u64,
    },
    CreateFilesystem {
        name: Name,
        format: Format,
        devices: Vec<Ref>,
        label: Label,
        profiles: Option<(BtrfsProfile, BtrfsProfile)>,
    },
    CreateSubvolume {
        filesystem: Name,
        path: SubvolumePath,
    },
    CreateSwap {
        name: Name,
        device: Ref,
        label: Label,
    },
}

struct ShowLabel<'a>(&'a Label);

impl fmt::Display for ShowLabel<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_none() {
            f.write_str("no label")
        } else {
            write!(f, "label {:?}", self.0.as_str())
        }
    }
}

fn join(refs: &[Ref]) -> String {
    refs.iter()
        .map(Ref::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operation::CreateTable {
                disk,
                device,
                table,
            } => write!(
                f,
                "create {table} partition table on disk.{disk} ({})",
                device.display()
            ),
            Operation::CreatePartition {
                disk,
                number,
                name,
                extent,
                kind,
                label,
            } => write!(
                f,
                "create partition {number} disk.{disk}.{name}: {} at {}, type {}, {}",
                format_size(extent.size),
                format_size(extent.start),
                kind.map_or("linux".to_owned(), |k| k.to_string()),
                ShowLabel(label)
            ),
            Operation::CreateLuks { name, device } => {
                write!(f, "create LUKS2 luks.{name} on {device}")
            }
            Operation::CreatePhysicalVolume { device } => {
                write!(f, "create physical volume on {device}")
            }
            Operation::CreateVolumeGroup { name, devices } => {
                write!(f, "create volume group {name} on {}", join(devices))
            }
            Operation::CreateLogicalVolume { vg, name, size } => {
                write!(
                    f,
                    "create logical volume lvm.{vg}.{name}: {}",
                    format_size(*size)
                )
            }
            Operation::CreateFilesystem {
                name,
                format,
                devices,
                label,
                profiles,
            } => {
                write!(
                    f,
                    "create {format} filesystem.{name} on {}, {}",
                    join(devices),
                    ShowLabel(label)
                )?;
                if let Some((data, metadata)) = profiles {
                    write!(f, ", data {data}, metadata {metadata}")?;
                }
                Ok(())
            }
            Operation::CreateSubvolume { filesystem, path } => {
                write!(
                    f,
                    "create subvolume {} in filesystem.{filesystem}",
                    path.as_str()
                )
            }
            Operation::CreateSwap {
                name,
                device,
                label,
            } => write!(f, "create swap.{name} on {device}, {}", ShowLabel(label)),
        }
    }
}

#[derive(Debug, Default)]
pub struct Plan {
    pub disks: BTreeMap<Name, BlockDevice>,
    pub layout: Layout,
    /// Groups of disks whose storage is already configured as declared,
    /// which apply leaves unchanged (ADR 0011).
    pub configured: Vec<BTreeSet<Name>>,
    pub operations: Vec<Operation>,
    /// Declared devices that nothing uses (ADR 0005).
    pub unused: Vec<Ref>,
    pub warnings: Vec<String>,
    /// Reasons that apply would refuse. The plan is usable only if empty.
    pub issues: Vec<Issue>,
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (name, d) in &self.disks {
            let unknown = || "unknown".to_owned();
            writeln!(
                f,
                "disk.{name}: {}, model {}, serial {}, {}",
                d.path.display(),
                d.model.clone().unwrap_or_else(unknown),
                d.serial.clone().unwrap_or_else(unknown),
                format_size(d.size)
            )?;
        }
        for group in &self.configured {
            let names: Vec<String> = group.iter().map(|n| format!("disk.{n}")).collect();
            writeln!(f, "already configured: {}", names.join(", "))?;
        }
        for (name, vg) in &self.layout.volume_groups {
            writeln!(
                f,
                "volume group {name}: capacity {}",
                format_size(vg.capacity)
            )?;
        }
        if !self.operations.is_empty() {
            writeln!(f, "operations:")?;
            for op in &self.operations {
                writeln!(f, "  {op}")?;
            }
        }
        if !self.unused.is_empty() {
            writeln!(f, "unused devices:")?;
            for r in &self.unused {
                writeln!(f, "  {r}")?;
            }
        }
        Ok(())
    }
}

pub fn plan(decl: &Declaration, sys: &dyn System) -> Plan {
    let mut plan = Plan::default();
    (plan.disks, plan.warnings, plan.issues) = resolve_disks(decl, sys);
    if !plan.issues.is_empty() {
        return plan;
    }

    let geometries = plan
        .disks
        .iter()
        .map(|(name, d)| {
            let g = Geometry {
                size: d.size,
                logical_sector: d.logical_sector,
            };
            (name.clone(), g)
        })
        .collect();
    let (layout, issues) = layout::compute(decl, &geometries);
    plan.layout = layout;
    plan.issues.extend(issues);

    for (name, luks) in &decl.luks {
        if let Err(e) = sys.check_keyfile(Path::new(luks.keyfile.as_str())) {
            plan.issues.push(Issue {
                at: format!("luks.{name}.keyfile"),
                message: format!("`{}` {e}", luks.keyfile.as_str()),
            });
        }
    }

    // ADR 0011: each group of connected disks is created if all of its
    // disks are empty, and left unchanged if it is configured as declared.
    let groups = groups(decl);
    let mut creatable = BTreeSet::new();
    for group in groups.values().collect::<BTreeSet<_>>() {
        let mut states = BTreeMap::new();
        for name in group {
            match sys.disk_state(&plan.disks[name]) {
                Ok(state) => {
                    states.insert(name, state);
                }
                Err(e) => plan.issues.push(Issue {
                    at: format!("disk.{name}"),
                    message: e,
                }),
            }
        }
        if states.len() < group.len() {
            continue;
        }
        if states.values().all(DiskState::is_empty) {
            creatable.extend(group.iter().cloned());
            continue;
        }
        let disks: Vec<GroupDisk> = states
            .iter()
            .map(|(name, state)| GroupDisk {
                name,
                device: &plan.disks[*name],
                state,
            })
            .collect();
        let issues = judge::judge(decl, sys, &plan.layout, &disks);
        if issues.is_empty() {
            plan.configured.push(group.clone());
            continue;
        }
        let names: Vec<String> = group
            .iter()
            .map(|n| format!("disk.{n} ({})", plan.disks[n].path.display()))
            .collect();
        let first = group.first().expect("a group has a disk");
        plan.issues.push(Issue {
            at: format!("disk.{first}"),
            message: if names.len() == 1 {
                format!(
                    "{} is neither empty nor configured as declared, so apply would refuse it \
                     (ADR 0011)",
                    names[0]
                )
            } else {
                format!(
                    "the group of {} is neither empty nor configured as declared, so apply \
                     would refuse it (ADR 0011)",
                    names.join(", ")
                )
            },
        });
        plan.issues.extend(issues);
    }

    check_existing_names(decl, sys, &creatable, &mut plan.issues);
    plan.operations = operations(decl, &plan.disks, &plan.layout, &creatable);
    plan.unused = unused(decl);
    plan
}

/// ADR 0003: resolves every disk of the declaration to a distinct device,
/// with warnings about fragile match conditions and the reasons that some
/// disks could not be resolved.
pub(crate) fn resolve_disks(
    decl: &Declaration,
    sys: &dyn System,
) -> (BTreeMap<Name, BlockDevice>, Vec<String>, Vec<Issue>) {
    let mut disks: BTreeMap<Name, BlockDevice> = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut issues = Vec::new();
    for (name, disk) in &decl.disk {
        if disk.matcher.path.is_kernel_name() {
            warnings.push(format!(
                "disk.{name}.match.path: `{}` is a kernel name, which may change between boots",
                disk.matcher.path.as_str()
            ));
        }
        match resolve(sys, &disk.matcher) {
            Ok(device) => {
                if let Some((other, _)) = disks.iter().find(|(_, d)| d.path == device.path) {
                    issues.push(Issue {
                        at: format!("disk.{name}.match"),
                        message: format!(
                            "resolves to {}, the same device as disk.{other}",
                            device.path.display()
                        ),
                    });
                } else {
                    disks.insert(name.clone(), device);
                }
            }
            Err(message) => issues.push(Issue {
                at: format!("disk.{name}.match"),
                message,
            }),
        }
    }
    (disks, warnings, issues)
}

/// ADR 0003: resolves the match conditions to exactly one whole disk.
fn resolve(sys: &dyn System, m: &Match) -> Result<BlockDevice, String> {
    let path = m.path.as_str();
    let (dir, file) = m.path.split();
    let candidates: Vec<PathBuf> = if file.contains(['*', '?', '[']) {
        let pattern = glob::Pattern::new(file).map_err(|e| format!("`{path}`: {e}"))?;
        let mut names = sys
            .read_dir(Path::new(dir))
            .map_err(|e| format!("cannot read {dir}: {e}"))?;
        names.sort();
        names
            .into_iter()
            .filter(|n| pattern.matches(n))
            .map(|n| Path::new(dir).join(n))
            .collect()
    } else {
        vec![PathBuf::from(path)]
    };

    // Paths that resolve to the same device count as one.
    let mut devices = BTreeSet::new();
    for candidate in &candidates {
        match sys.canonicalize(candidate) {
            Ok(p) => {
                devices.insert(p);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot resolve {}: {e}", candidate.display())),
        }
    }
    if devices.is_empty() {
        return Err(format!("no device matches `{path}`"));
    }

    let mut others = Vec::new();
    let mut disks = Vec::new();
    for p in devices {
        let device = sys
            .block_device(&p)
            .map_err(|e| format!("cannot read {}: {e}", p.display()))?;
        match device {
            Some(d) if d.kind == DeviceKind::Disk => disks.push(d),
            Some(d) => others.push(format!("{} ({:?})", p.display(), d.kind).to_lowercase()),
            None => others.push(format!("{} (not a block device)", p.display())),
        }
    }
    if disks.is_empty() {
        return Err(format!(
            "`{path}` matches no whole disk, only {}",
            others.join(", ")
        ));
    }

    let (fit, small): (Vec<_>, Vec<_>) = disks
        .into_iter()
        .partition(|d| m.min_size.is_none_or(|min| d.size >= min.0));
    match <[_; 1]>::try_from(fit) {
        Ok([disk]) => Ok(disk),
        Err(fit) if fit.is_empty() => Err(format!(
            "`{path}` matches only disks smaller than min_size {}: {}",
            format_size(m.min_size.map_or(0, |m| m.0)),
            small
                .iter()
                .map(|d| format!("{} ({})", d.path.display(), format_size(d.size)))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Err(fit) => Err(format!(
            "`{path}` matches {} disks, but must match exactly one: {}",
            fit.len(),
            fit.iter()
                .map(|d| d.path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The disks that a device is built on.
pub(crate) fn disks_of(decl: &Declaration, r: &Ref) -> BTreeSet<Name> {
    match r {
        Ref::Partition { disk, .. } => BTreeSet::from([disk.clone()]),
        Ref::Luks { name } => disks_of(decl, &decl.luks[name].device),
        Ref::LogicalVolume { vg, .. } => decl.lvm[vg]
            .devices
            .iter()
            .flat_map(|d| disks_of(decl, d))
            .collect(),
    }
}

/// ADR 0011: maps each disk to the group of disks connected to it through
/// the elements built on them.
fn groups(decl: &Declaration) -> BTreeMap<Name, BTreeSet<Name>> {
    let mut groups: BTreeMap<Name, BTreeSet<Name>> = decl
        .disk
        .keys()
        .map(|d| (d.clone(), BTreeSet::from([d.clone()])))
        .collect();
    let users = decl
        .luks
        .values()
        .map(|l| vec![l.device.clone()])
        .chain(decl.lvm.values().map(|g| g.devices.clone()))
        .chain(decl.filesystem.values().map(|fs| {
            fs.device
                .iter()
                .chain(fs.devices.iter().flatten())
                .cloned()
                .collect()
        }))
        .chain(decl.swap.values().map(|s| vec![s.device.clone()]));
    for devices in users {
        let disks: BTreeSet<Name> = devices.iter().flat_map(|r| disks_of(decl, r)).collect();
        let merged: BTreeSet<Name> = disks
            .iter()
            .flat_map(|d| groups[d].iter().cloned())
            .collect();
        for d in &merged {
            groups.insert(d.clone(), merged.clone());
        }
    }
    groups
}

/// ADR 0005: names that appear on the system must not already exist there.
/// A group that will not be created is not checked, because apply leaves
/// it alone.
fn check_existing_names(
    decl: &Declaration,
    sys: &dyn System,
    creatable: &BTreeSet<Name>,
    issues: &mut Vec<Issue>,
) {
    let created = |r: &Ref| disks_of(decl, r).iter().all(|d| creatable.contains(d));
    let vgs: Vec<&Name> = decl
        .lvm
        .iter()
        .filter(|(_, g)| g.devices.iter().all(created))
        .map(|(name, _)| name)
        .collect();
    if !vgs.is_empty() {
        match sys.volume_groups() {
            Ok(existing) => {
                for name in vgs {
                    if existing.contains_key(name.as_str()) {
                        issues.push(Issue {
                            at: format!("lvm.{name}"),
                            message: format!("a volume group named `{name}` already exists"),
                        });
                    }
                }
            }
            Err(e) => issues.push(Issue {
                at: "lvm".to_owned(),
                message: e,
            }),
        }
    }

    let mut mappers: Vec<(String, String)> = decl
        .luks
        .iter()
        .filter(|(_, l)| created(&l.device))
        .map(|(name, _)| (name.to_string(), format!("luks.{name}")))
        .collect();
    for (vg, g) in &decl.lvm {
        if g.devices.iter().all(created) {
            let escape = |s: &str| s.replace('-', "--");
            for lv in g.volumes.keys() {
                mappers.push((
                    format!("{}-{}", escape(vg.as_str()), escape(lv.as_str())),
                    format!("lvm.{vg}.volumes.{lv}"),
                ));
            }
        }
    }
    if !mappers.is_empty() {
        match sys.mapper_names() {
            Ok(existing) => {
                for (mapper, at) in mappers {
                    if existing.contains(&mapper) {
                        issues.push(Issue {
                            at,
                            message: format!(
                                "a device-mapper device named `{mapper}` already exists"
                            ),
                        });
                    }
                }
            }
            Err(e) => issues.push(Issue {
                at: "luks".to_owned(),
                message: e,
            }),
        }
    }
}

/// The operations that create the elements on creatable disks, each after
/// the devices it is built on.
fn operations(
    decl: &Declaration,
    disks: &BTreeMap<Name, BlockDevice>,
    layout: &Layout,
    creatable: &BTreeSet<Name>,
) -> Vec<Operation> {
    let mut b = Builder {
        decl,
        layout,
        ops: Vec::new(),
        done_luks: BTreeSet::new(),
        done_vgs: BTreeSet::new(),
    };
    for (name, disk) in &decl.disk {
        if !creatable.contains(name) {
            continue;
        }
        let Some(extents) = layout.partitions.get(name) else {
            continue;
        };
        b.ops.push(Operation::CreateTable {
            disk: name.clone(),
            device: disks[name].path.clone(),
            table: disk.table,
        });
        for (i, (p, extent)) in disk.partitions.iter().zip(extents).enumerate() {
            b.ops.push(Operation::CreatePartition {
                disk: name.clone(),
                number: i + 1,
                name: p.name.clone(),
                extent: *extent,
                kind: p.kind,
                label: p.label.clone(),
            });
        }
    }
    let created = |r: &Ref| {
        disks_of(decl, r).iter().all(|d| creatable.contains(d))
            && layout.device_size(decl, r).is_some()
    };
    for (name, luks) in &decl.luks {
        if created(&luks.device) {
            b.luks(name);
        }
    }
    for (name, vg) in &decl.lvm {
        if vg.devices.iter().all(created) {
            b.volume_group(name);
        }
    }
    for (name, fs) in &decl.filesystem {
        let devices: Vec<Ref> = fs
            .device
            .iter()
            .chain(fs.devices.iter().flatten())
            .cloned()
            .collect();
        if !devices.iter().all(created) {
            continue;
        }
        for d in &devices {
            b.device(d);
        }
        b.ops.push(Operation::CreateFilesystem {
            name: name.clone(),
            format: fs.format,
            devices,
            label: fs.label.clone(),
            profiles: fs
                .btrfs
                .as_ref()
                .map(|o| (o.data_profile, o.metadata_profile)),
        });
        for sub in fs.subvolumes.iter().flat_map(|s| s.values()) {
            b.ops.push(Operation::CreateSubvolume {
                filesystem: name.clone(),
                path: sub.path.clone(),
            });
        }
    }
    for (name, swap) in &decl.swap {
        if created(&swap.device) {
            b.device(&swap.device);
            b.ops.push(Operation::CreateSwap {
                name: name.clone(),
                device: swap.device.clone(),
                label: swap.label.clone(),
            });
        }
    }
    b.ops
}

struct Builder<'a> {
    decl: &'a Declaration,
    layout: &'a Layout,
    ops: Vec<Operation>,
    done_luks: BTreeSet<Name>,
    done_vgs: BTreeSet<Name>,
}

impl Builder<'_> {
    /// Adds the operations that create a device, once.
    fn device(&mut self, r: &Ref) {
        match r {
            // Partitions are created with their disk.
            Ref::Partition { .. } => {}
            Ref::Luks { name } => self.luks(name),
            Ref::LogicalVolume { vg, .. } => self.volume_group(vg),
        }
    }

    fn luks(&mut self, name: &Name) {
        if !self.done_luks.insert(name.clone()) {
            return;
        }
        let device = &self.decl.luks[name].device;
        self.device(device);
        self.ops.push(Operation::CreateLuks {
            name: name.clone(),
            device: device.clone(),
        });
    }

    fn volume_group(&mut self, name: &Name) {
        if !self.done_vgs.insert(name.clone()) {
            return;
        }
        let vg = &self.decl.lvm[name];
        for d in &vg.devices {
            self.device(d);
        }
        for d in &vg.devices {
            self.ops
                .push(Operation::CreatePhysicalVolume { device: d.clone() });
        }
        self.ops.push(Operation::CreateVolumeGroup {
            name: name.clone(),
            devices: vg.devices.clone(),
        });
        let Some(layout) = self.layout.volume_groups.get(name) else {
            return;
        };
        for (lv, size) in &layout.volumes {
            self.ops.push(Operation::CreateLogicalVolume {
                vg: name.clone(),
                name: lv.clone(),
                size: *size,
            });
        }
    }
}

/// ADR 0005: declared devices that no element uses.
pub(crate) fn unused(decl: &Declaration) -> Vec<Ref> {
    let used: BTreeSet<&Ref> = decl
        .luks
        .values()
        .map(|l| &l.device)
        .chain(decl.lvm.values().flat_map(|g| &g.devices))
        .chain(
            decl.filesystem
                .values()
                .flat_map(|fs| fs.device.iter().chain(fs.devices.iter().flatten())),
        )
        .chain(decl.swap.values().map(|s| &s.device))
        .collect();
    let partitions = decl.disk.iter().flat_map(|(disk, d)| {
        d.partitions.iter().map(|p| Ref::Partition {
            disk: disk.clone(),
            partition: p.name.clone(),
        })
    });
    let luks = decl
        .luks
        .keys()
        .map(|name| Ref::Luks { name: name.clone() });
    let lvs = decl.lvm.iter().flat_map(|(vg, g)| {
        g.volumes.keys().map(|lv| Ref::LogicalVolume {
            vg: vg.clone(),
            lv: lv.clone(),
        })
    });
    partitions
        .chain(luks)
        .chain(lvs)
        .filter(|r| !used.contains(r))
        .collect()
}

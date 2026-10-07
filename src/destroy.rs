//! Destroying (ADR 0004, ADR 0016): reads what is on the target disks,
//! refuses what it cannot safely tear down, lists the steps that erase the
//! existing storage, and performs them. Reading goes through `System` and
//! changing through `Change`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::change::Change;
use crate::layout::format_size;
use crate::model::{Declaration, Name};
use crate::plan::resolve_disks;
use crate::system::{BlockDevice, Node, NodeKind, Probe, System, VolumeGroupState};
use crate::validate::Issue;

/// The disks that `destroy` erases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Targets {
    All,
    /// The disks given with `--target`, such as `disk.data0`.
    Disks(Vec<String>),
}

/// One step of destroy, in the order destroy performs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    DeactivateVolumeGroup {
        name: String,
    },
    CloseLuks {
        name: String,
        mapping: PathBuf,
        device: PathBuf,
    },
    /// Erases the signatures on a partition.
    Wipe {
        device: PathBuf,
        signatures: Vec<String>,
    },
    /// Erases the partition table and the signatures on a disk itself.
    WipeDisk {
        disk: Name,
        device: PathBuf,
        signatures: Vec<String>,
    },
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Step::DeactivateVolumeGroup { name } => {
                write!(f, "deactivate volume group {name}")
            }
            Step::CloseLuks {
                name,
                mapping,
                device,
            } => write!(
                f,
                "close LUKS mapping {name} ({}) on {}",
                mapping.display(),
                device.display()
            ),
            Step::Wipe { device, signatures } => {
                write!(f, "wipe {} on {}", signatures.join(", "), device.display())
            }
            Step::WipeDisk {
                disk,
                device,
                signatures,
            } => write!(
                f,
                "wipe {} on disk.{disk} ({})",
                signatures.join(", "),
                device.display()
            ),
        }
    }
}

#[derive(Debug, Default)]
pub struct Destruction {
    /// The target disks.
    pub disks: BTreeMap<Name, BlockDevice>,
    pub steps: Vec<Step>,
    /// Members of the storage on the targets that this machine lacks.
    pub missing: Vec<String>,
    pub warnings: Vec<String>,
    /// Reasons that destroy refuses. Nothing may be done unless empty.
    pub issues: Vec<Issue>,
}

impl Destruction {
    /// Whether another reading finds the same disks and steps, so that
    /// what the user confirmed is still what would be done (ADR 0016).
    pub fn same_as(&self, other: &Destruction) -> bool {
        other.issues.is_empty()
            && self.disks == other.disks
            && self.steps == other.steps
            && self.missing == other.missing
    }
}

impl fmt::Display for Destruction {
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
        if self.steps.is_empty() {
            if self.issues.is_empty() {
                writeln!(f, "nothing to destroy")?;
            }
        } else {
            writeln!(f, "operations:")?;
            for step in &self.steps {
                writeln!(f, "  {step}")?;
            }
        }
        if !self.missing.is_empty() {
            writeln!(f, "missing members:")?;
            for m in &self.missing {
                writeln!(f, "  {m}")?;
            }
        }
        Ok(())
    }
}

pub fn destroy(decl: &Declaration, sys: &dyn System, targets: &Targets) -> Destruction {
    let mut d = Destruction::default();
    let (disks, warnings, issues) = resolve_disks(decl, sys);
    d.warnings = warnings;
    d.issues = issues;
    let names: BTreeSet<&Name> = match targets {
        Targets::All => decl.disk.keys().collect(),
        Targets::Disks(list) => list
            .iter()
            .filter_map(|t| {
                let name = t
                    .strip_prefix("disk.")
                    .and_then(|n| decl.disk.keys().find(|k| k.as_str() == n));
                if name.is_none() {
                    d.issues.push(Issue {
                        at: "--target".to_owned(),
                        message: format!(
                            "`{t}` is not a disk of the declaration, such as `disk.<name>`"
                        ),
                    });
                }
                name
            })
            .collect(),
    };
    if !d.issues.is_empty() {
        return d;
    }
    d.disks = names
        .into_iter()
        .map(|n| (n.clone(), disks[n].clone()))
        .collect();

    let mut r = Reader {
        sys,
        disks: &disks,
        targets: d.disks.values().map(|b| b.path.clone()).collect(),
        nodes: BTreeMap::new(),
        volume_groups: None,
        issues: Vec::new(),
    };
    let uses = match sys.uses() {
        Ok(uses) => uses,
        Err(e) => {
            d.issues.push(Issue {
                at: "destroy".to_owned(),
                message: e,
            });
            return d;
        }
    };

    let mut wipes = Vec::new();
    let mut disk_wipes = Vec::new();
    // The stack of devices on every target, and what is checked once.
    let mut stack = Vec::new();
    let mut vgs: BTreeMap<String, Name> = BTreeMap::new();
    let mut btrfs: BTreeMap<String, Name> = BTreeMap::new();
    for (name, disk) in &d.disks {
        let Some(scan) = r.scan(name, disk) else {
            continue;
        };
        for device in &scan.devices {
            let at = format!("disk.{name}");
            if let Some(usage) = uses.get(device) {
                r.issue(
                    &at,
                    format!(
                        "{} is {usage}; destroy does not stop what is in use, so stop it \
                         first (ADR 0004)",
                        device.display()
                    ),
                );
            }
            let node = r.nodes[device].clone();
            match &node.kind {
                NodeKind::Md => r.issue(
                    &at,
                    format!(
                        "{} is an md array, which destroy does not support yet (ADR 0016)",
                        device.display()
                    ),
                ),
                NodeKind::OtherMapper { name, uuid } => r.issue(
                    &at,
                    format!(
                        "{} is the device-mapper device `{name}` of an unknown kind (UUID \
                         `{uuid}`), which destroy does not tear down (ADR 0004)",
                        device.display()
                    ),
                ),
                NodeKind::LogicalVolume { name: lv } => match r.group_of_volume(device) {
                    Some(vg) => {
                        vgs.entry(vg).or_insert_with(|| name.clone());
                    }
                    None => r.issue(
                        &at,
                        format!("cannot find the volume group of the active logical volume `{lv}`"),
                    ),
                },
                _ => {}
            }
            let Some(probe) = r.probe(&at, device) else {
                continue;
            };
            match probe.kind.as_deref() {
                Some("linux_raid_member") => r.issue(
                    &at,
                    format!(
                        "{} is a member of an md array, which destroy does not support yet \
                         (ADR 0016)",
                        device.display()
                    ),
                ),
                Some("LVM2_member") => {
                    if let Some(vg) = r.group_of_volume_device(device) {
                        vgs.entry(vg).or_insert_with(|| name.clone());
                    }
                }
                Some("btrfs") => match probe.uuid.clone() {
                    Some(uuid) => {
                        btrfs.entry(uuid).or_insert_with(|| name.clone());
                    }
                    None => r.issue(&at, format!("btrfs on {} has no UUID", device.display())),
                },
                _ => {}
            }
            if scan.partitions.contains(device) {
                let signatures = signatures(&probe);
                if !signatures.is_empty() {
                    wipes.push(Step::Wipe {
                        device: device.clone(),
                        signatures,
                    });
                }
            }
        }
        if !scan.signatures.is_empty() || !scan.partitions.is_empty() {
            disk_wipes.push(Step::WipeDisk {
                disk: name.clone(),
                device: disk.path.clone(),
                signatures: scan.signatures,
            });
        }
        stack.extend(scan.devices);
    }

    // ADR 0004: the members of storage that spans several disks must all be
    // on the targets.
    for (vg, at) in &vgs {
        let Some(state) = r.volume_groups().and_then(|g| g.get(vg).cloned()) else {
            continue;
        };
        let what = format!("the volume group `{vg}`");
        for pv in &state.devices {
            if pv.as_os_str() == "[unknown]" {
                d.missing
                    .push(format!("{what} lacks some of its physical volumes"));
            } else {
                r.member(at, &what, pv);
            }
        }
    }
    for (uuid, at) in &btrfs {
        match sys.btrfs_members(uuid) {
            Ok(members) => {
                let what = format!("the btrfs {uuid}");
                for device in &members.devices {
                    r.member(at, &what, device);
                }
                let found = members.devices.len() as u64;
                if found < members.count {
                    d.missing.push(format!(
                        "{what} lacks {} of its {} devices",
                        members.count - found,
                        members.count
                    ));
                }
            }
            Err(e) => r.issue(&format!("disk.{at}"), e),
        }
    }

    d.issues.append(&mut r.issues);
    if !d.issues.is_empty() {
        return d;
    }
    match r.teardown(&stack) {
        Ok(steps) => d.steps = steps,
        Err(e) => {
            d.issues.push(Issue {
                at: "destroy".to_owned(),
                message: e,
            });
            return d;
        }
    }
    d.steps.extend(wipes);
    d.steps.extend(disk_wipes);
    d
}

/// What `wipefs` would erase on a device, as blkid names it.
fn signatures(probe: &Probe) -> Vec<String> {
    let kind = probe.kind.as_ref().map(|k| match &probe.version {
        Some(v) => format!("{k} (version {v}) signature"),
        None => format!("{k} signature"),
    });
    let table = probe.table.as_ref().map(|t| format!("{t} partition table"));
    table.into_iter().chain(kind).collect()
}

/// What is on a target disk.
struct Scan {
    /// The disk, its partitions and every device built on them.
    devices: Vec<PathBuf>,
    partitions: BTreeSet<PathBuf>,
    /// The partition table and signatures on the disk itself.
    signatures: Vec<String>,
}

struct Reader<'a> {
    sys: &'a dyn System,
    disks: &'a BTreeMap<Name, BlockDevice>,
    /// The canonical paths of the target disks.
    targets: BTreeSet<PathBuf>,
    nodes: BTreeMap<PathBuf, Node>,
    volume_groups: Option<Result<BTreeMap<String, VolumeGroupState>, String>>,
    issues: Vec<Issue>,
}

impl Reader<'_> {
    fn issue(&mut self, at: &str, message: String) {
        let issue = Issue {
            at: at.to_owned(),
            message,
        };
        if !self.issues.contains(&issue) {
            self.issues.push(issue);
        }
    }

    fn scan(&mut self, name: &Name, disk: &BlockDevice) -> Option<Scan> {
        let at = format!("disk.{name}");
        let state = match self.sys.disk_state(disk) {
            Ok(state) => state,
            Err(e) => {
                self.issue(&at, e);
                return None;
            }
        };
        let table = if state.is_empty() {
            None
        } else {
            match self.sys.partition_table(disk) {
                Ok(table) => table,
                Err(e) => {
                    self.issue(&at, e);
                    return None;
                }
            }
        };
        let partitions: BTreeSet<PathBuf> = table
            .iter()
            .flat_map(|t| &t.partitions)
            .map(|p| p.device.clone())
            .collect();
        if partitions.len() != state.partitions.len() {
            // ADR 0016: signatures inside partitions that the kernel does
            // not know cannot be erased through their devices.
            let device = disk.path.display();
            self.issue(
                &at,
                format!(
                    "the partitions that the kernel knows on {device} differ from the \
                     partition table on it; have the kernel re-read the table, for example \
                     with `partx --update {device}`"
                ),
            );
            return None;
        }

        let mut devices = Vec::new();
        let mut queue: Vec<PathBuf> = std::iter::once(disk.path.clone())
            .chain(partitions.iter().cloned())
            .collect();
        while let Some(device) = queue.pop() {
            if devices.contains(&device) {
                continue;
            }
            let node = match self.node(&device) {
                Ok(node) => node,
                Err(e) => {
                    self.issue(&at, e);
                    return None;
                }
            };
            queue.extend(node.holders.iter().cloned());
            devices.push(device);
        }
        devices.sort();
        Some(Scan {
            devices,
            partitions,
            signatures: state.signatures,
        })
    }

    fn node(&mut self, device: &Path) -> Result<Node, String> {
        if let Some(node) = self.nodes.get(device) {
            return Ok(node.clone());
        }
        let node = self.sys.node(device)?;
        self.nodes.insert(device.to_owned(), node.clone());
        Ok(node)
    }

    fn probe(&mut self, at: &str, device: &Path) -> Option<Probe> {
        match self.sys.probe(device) {
            Ok(probe) => Some(probe),
            Err(e) => {
                self.issue(at, e);
                None
            }
        }
    }

    /// The volume groups, read once; `None` after reporting an error.
    fn volume_groups(&mut self) -> Option<&BTreeMap<String, VolumeGroupState>> {
        let sys = self.sys;
        let groups = self
            .volume_groups
            .get_or_insert_with(|| sys.volume_groups());
        if let Err(e) = groups {
            let e = e.clone();
            self.issue("lvm", e);
            return None;
        }
        self.volume_groups.as_ref()?.as_ref().ok()
    }

    /// The volume group that a physical volume belongs to.
    fn group_of_volume_device(&mut self, pv: &Path) -> Option<String> {
        self.volume_groups()?
            .iter()
            .find(|(_, g)| g.devices.contains(pv))
            .map(|(name, _)| name.clone())
    }

    /// The volume group of an active logical volume.
    fn group_of_volume(&mut self, lv: &Path) -> Option<String> {
        self.volume_groups()?
            .iter()
            .find(|(_, g)| g.volumes.values().any(|v| v.device.as_deref() == Some(lv)))
            .map(|(name, _)| name.clone())
    }

    /// The disks that a device is built on.
    fn disks_under(&mut self, device: &Path) -> Result<BTreeSet<PathBuf>, String> {
        let node = self.node(device)?;
        if node.kind == NodeKind::Disk {
            return Ok(BTreeSet::from([device.to_owned()]));
        }
        let mut disks = BTreeSet::new();
        for lower in &node.lower {
            disks.extend(self.disks_under(lower)?);
        }
        Ok(disks)
    }

    /// ADR 0004: refuses a member of storage on a target that is on a disk
    /// that is not a target.
    fn member(&mut self, target: &Name, what: &str, device: &Path) {
        let at = format!("disk.{target}");
        let disks = match self.disks_under(device) {
            Ok(disks) => disks,
            Err(e) => {
                self.issue(&at, e);
                return;
            }
        };
        for disk in disks.difference(&self.targets.clone()) {
            let declared = self.disks.iter().find(|(_, b)| &b.path == disk);
            let message = match declared {
                Some((other, _)) => format!(
                    "{what} on it is also on {} of disk.{other} ({}); add `--target \
                     disk.{other}` or use `--all` (ADR 0004)",
                    device.display(),
                    disk.display()
                ),
                None => format!(
                    "{what} on it is also on {} of {}, which is not in the declaration; \
                     add the disk to the declaration (ADR 0004)",
                    device.display(),
                    disk.display()
                ),
            };
            self.issue(&at, message);
        }
    }

    /// ADR 0016: deactivates the volume groups and closes the LUKS mappings
    /// in the stacks, each after everything built on it.
    fn teardown(&mut self, stack: &[PathBuf]) -> Result<Vec<Step>, String> {
        // Each unit is torn down by one step, with the devices it removes.
        let mut units: Vec<(Step, BTreeSet<PathBuf>)> = Vec::new();
        for device in stack {
            let node = self.nodes[device].clone();
            match node.kind {
                NodeKind::Luks { name } => {
                    let step = Step::CloseLuks {
                        name,
                        mapping: device.clone(),
                        device: node.lower.first().cloned().unwrap_or_default(),
                    };
                    units.push((step, BTreeSet::from([device.clone()])));
                }
                NodeKind::LogicalVolume { .. } => {
                    let name = self.group_of_volume(device).unwrap_or_default();
                    let step = Step::DeactivateVolumeGroup { name };
                    match units.iter_mut().find(|(s, _)| *s == step) {
                        Some((_, devices)) => {
                            devices.insert(device.clone());
                        }
                        None => units.push((step, BTreeSet::from([device.clone()]))),
                    }
                }
                _ => {}
            }
        }
        let mut gone: BTreeSet<PathBuf> = BTreeSet::new();
        let mut steps = Vec::new();
        while !units.is_empty() {
            let ready = units.iter().position(|(_, devices)| {
                devices.iter().all(|d| {
                    self.nodes[d]
                        .holders
                        .iter()
                        .all(|h| gone.contains(h) || devices.contains(h))
                })
            });
            let Some(i) = ready else {
                return Err("cannot find an order to tear down the devices".to_owned());
            };
            let (step, devices) = units.remove(i);
            gone.extend(devices);
            steps.push(step);
        }
        Ok(steps)
    }
}

/// Performs the steps in order, calling `done` after each, and checks the
/// state after each step. Stops at the first failure (ADR 0016).
pub fn execute(
    d: &Destruction,
    sys: &dyn System,
    change: &dyn Change,
    done: &mut dyn FnMut(&Step),
) -> Result<(), String> {
    for step in &d.steps {
        perform(step, d, sys, change).map_err(|e| format!("{step}: {e}"))?;
        done(step);
    }
    Ok(())
}

fn perform(
    step: &Step,
    d: &Destruction,
    sys: &dyn System,
    change: &dyn Change,
) -> Result<(), String> {
    match step {
        Step::DeactivateVolumeGroup { name } => {
            change.deactivate_volume_group(name)?;
            let groups = sys.volume_groups()?;
            let active = groups
                .get(name)
                .is_some_and(|g| g.volumes.values().any(|v| v.device.is_some()));
            if active {
                return Err("some of its logical volumes are still active".to_owned());
            }
        }
        Step::CloseLuks { name, device, .. } => {
            change.close_luks(name)?;
            if sys.luks_mapping(device)?.is_some() {
                return Err(format!("{} is still open", device.display()));
            }
        }
        Step::Wipe { device, .. } => {
            change.wipe(device)?;
            let probe = sys.probe(device)?;
            if probe.kind.is_some() || probe.table.is_some() {
                let left = signatures(&probe).join(", ");
                return Err(format!("{} still has {left}", device.display()));
            }
        }
        Step::WipeDisk { disk, .. } => {
            let disk = &d.disks[disk];
            change.wipe(&disk.path)?;
            if !sys.disk_state(disk)?.partitions.is_empty() {
                change.forget_partitions(&disk.path)?;
            }
            let state = sys.disk_state(disk)?;
            if !state.is_empty() {
                return Err(format!(
                    "{} is not empty: {}",
                    disk.path.display(),
                    describe(&state)
                ));
            }
        }
    }
    Ok(())
}

/// What is on a disk, for reports.
pub fn describe(state: &crate::system::DiskState) -> String {
    if state.is_empty() {
        return "empty".to_owned();
    }
    let mut parts = state.signatures.clone();
    if !state.partitions.is_empty() {
        parts.push(format!("partitions {}", state.partitions.join(", ")));
    }
    if !state.holders.is_empty() {
        parts.push(format!("used by {}", state.holders.join(", ")));
    }
    parts.join("; ")
}

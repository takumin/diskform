//! Mounting and unmounting (ADR 0004, ADR 0019). `mount` opens the LUKS
//! devices and activates the volume groups that the judgment of ADR 0011
//! could not read past, and mounts the filesystems under a root directory
//! once nothing differs from the declaration. `unmount` undoes that, for
//! the declared storage only. Reading goes through `System` and changing
//! through `Activate`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::change::Activate;
use crate::judge::{self, GroupDisk, Judgment, Unread};
use crate::layout::{self, Geometry, Layout};
use crate::model::{Declaration, Format, Name, Ref};
use crate::plan::{groups, resolve_disks};
use crate::system::{BlockDevice, MountEntry, NodeKind, System};
use crate::validate::Issue;

/// A declared filesystem or subvolume that has a mount point.
struct Target {
    /// `filesystem.<name>` or `filesystem.<name>.subvolumes.<name>`.
    what: String,
    filesystem: Name,
    mount: String,
    format: Format,
    devices: Vec<Ref>,
    options: Vec<String>,
    subvolume: Option<String>,
}

/// The mount points of the declaration, each after those it is under.
fn targets(decl: &Declaration) -> Vec<Target> {
    let mut targets = Vec::new();
    for (name, fs) in &decl.filesystem {
        let devices: Vec<Ref> = fs
            .device
            .iter()
            .chain(fs.devices.iter().flatten())
            .cloned()
            .collect();
        let options = fs.mount_options.clone().unwrap_or_default();
        if let Some(mount) = &fs.mount {
            targets.push(Target {
                what: format!("filesystem.{name}"),
                filesystem: name.clone(),
                mount: mount.as_str().to_owned(),
                format: fs.format,
                devices: devices.clone(),
                options: options.clone(),
                subvolume: None,
            });
        }
        for (sub, s) in fs.subvolumes.iter().flatten() {
            let Some(mount) = &s.mount else {
                continue;
            };
            let mut options = options.clone();
            options.push(format!("subvol={}", s.path.as_str()));
            targets.push(Target {
                what: format!("filesystem.{name}.subvolumes.{sub}"),
                filesystem: name.clone(),
                mount: mount.as_str().to_owned(),
                format: fs.format,
                devices: devices.clone(),
                options,
                subvolume: Some(s.path.as_str().to_owned()),
            });
        }
    }
    // A path sorts after the paths it is under.
    targets.sort_by(|a, b| a.mount.cmp(&b.mount));
    targets
}

/// Where a mount point of the declaration is under `root`.
fn under(root: &Path, mount: &str) -> PathBuf {
    root.join(mount.trim_start_matches('/'))
}

fn fail(message: String) -> Vec<Issue> {
    vec![Issue {
        at: "mount".to_owned(),
        message,
    }]
}

/// The canonical path of the root directory.
fn root_dir(root: &Path) -> Result<PathBuf, Vec<Issue>> {
    let shown = root.display();
    let root = fs::canonicalize(root).map_err(|e| fail(format!("--root {shown}: {e}")))?;
    if !root.is_dir() {
        return Err(fail(format!("--root {shown} is not a directory")));
    }
    Ok(root)
}

/// The disks that a device is built on, by following `System::node` down.
fn disks_under(sys: &dyn System, device: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let node = sys.node(device)?;
    if node.kind == NodeKind::Disk {
        return Ok(BTreeSet::from([device.to_owned()]));
    }
    let mut disks = BTreeSet::new();
    for lower in &node.lower {
        disks.extend(disks_under(sys, lower)?);
    }
    Ok(disks)
}

/// The matched disks, the layout and the groups of a declaration.
struct Resolved {
    disks: BTreeMap<Name, BlockDevice>,
    layout: Layout,
    groups: BTreeSet<BTreeSet<Name>>,
}

fn resolve(
    decl: &Declaration,
    sys: &dyn System,
    warn: &mut dyn FnMut(String),
) -> Result<Resolved, Vec<Issue>> {
    let (disks, warnings, issues) = resolve_disks(decl, sys);
    for w in warnings {
        warn(w);
    }
    if !issues.is_empty() {
        return Err(issues);
    }
    let geometries = disks
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
    if !issues.is_empty() {
        return Err(issues);
    }
    let groups = groups(decl).into_values().collect();
    Ok(Resolved {
        disks,
        layout,
        groups,
    })
}

/// Judges every group. A group whose disks are all empty has nothing to
/// mount, so it is refused like any group that differs.
fn judge_all(
    decl: &Declaration,
    sys: &dyn System,
    r: &Resolved,
) -> Result<Vec<Judgment>, Vec<Issue>> {
    let mut judgments = Vec::new();
    let mut issues = Vec::new();
    for group in &r.groups {
        let mut states = BTreeMap::new();
        for name in group {
            match sys.disk_state(&r.disks[name]) {
                Ok(state) => {
                    states.insert(name, state);
                }
                Err(e) => issues.push(Issue {
                    at: format!("disk.{name}"),
                    message: e,
                }),
            }
        }
        if states.len() < group.len() {
            continue;
        }
        if states.values().all(|s| s.is_empty()) {
            let names: Vec<String> = group.iter().map(|n| format!("disk.{n}")).collect();
            issues.push(Issue {
                at: format!("disk.{}", group.first().expect("a group has a disk")),
                message: format!(
                    "{} {} empty; create the storage with apply first",
                    names.join(", "),
                    if names.len() == 1 { "is" } else { "are" }
                ),
            });
            continue;
        }
        let disks: Vec<GroupDisk> = states
            .iter()
            .map(|(name, state)| GroupDisk {
                name,
                device: &r.disks[*name],
                state,
            })
            .collect();
        judgments.push(judge::judge(decl, sys, &r.layout, &disks));
    }
    if issues.is_empty() {
        Ok(judgments)
    } else {
        Err(issues)
    }
}

/// ADR 0019: opens the LUKS devices, activates the volume groups and mounts
/// the filesystems under `root`, calling `done` after each change.
pub fn mount(
    decl: &Declaration,
    sys: &dyn System,
    act: &dyn Activate,
    root: &Path,
    warn: &mut dyn FnMut(String),
    done: &mut dyn FnMut(String),
) -> Result<(), Vec<Issue>> {
    let root = root_dir(root)?;
    let r = resolve(decl, sys, warn)?;

    // Each round opens what the judgment could not read past, so that the
    // next round reads the layer above it. A layer is never opened before
    // everything below it has been found as declared.
    let mut devices = BTreeMap::new();
    for round in 0.. {
        let mut issues = Vec::new();
        let mut unread = Vec::new();
        let mut found = BTreeMap::new();
        for j in judge_all(decl, sys, &r)? {
            issues.extend(j.issues);
            unread.extend(
                j.unread
                    .into_iter()
                    .map(|(u, _)| u)
                    .filter(|u| !matches!(u, Unread::UnmountedBtrfs { .. })),
            );
            found.extend(j.devices);
        }
        if !issues.is_empty() {
            return Err(issues);
        }
        if unread.is_empty() {
            devices = found;
            break;
        }
        if round == 3 {
            return Err(fail("the storage stays closed after opening it".to_owned()));
        }
        let mut activated = BTreeSet::new();
        for u in &unread {
            match u {
                Unread::ClosedLuks { name, device } => {
                    let keyfile = decl.luks[name].keyfile.as_str();
                    sys.check_keyfile(Path::new(keyfile))
                        .map_err(|e| fail(format!("`{keyfile}` {e}")))?;
                    let step = format!("open luks.{name} ({}) as {name}", device.display());
                    act.open_luks(device, Path::new(keyfile), name.as_str())
                        .map_err(|e| fail(format!("{step}: {e}")))?;
                    if sys.luks_mapping(device).map_err(fail)?.is_none() {
                        return Err(fail(format!("{step}: it is not open")));
                    }
                    done(step);
                }
                Unread::InactiveVolume { vg } => {
                    if !activated.insert(vg) {
                        continue;
                    }
                    let step = format!("activate volume group {vg}");
                    act.activate_volume_group(vg.as_str())
                        .map_err(|e| fail(format!("{step}: {e}")))?;
                    let groups = sys.volume_groups().map_err(fail)?;
                    let inactive = groups
                        .get(vg.as_str())
                        .is_none_or(|g| g.volumes.values().any(|v| v.device.is_none()));
                    if inactive {
                        return Err(fail(format!(
                            "{step}: some of its logical volumes are not active"
                        )));
                    }
                    done(step);
                }
                Unread::UnmountedBtrfs { .. } => {}
            }
        }
    }

    let mut checked = BTreeSet::new();
    for t in targets(decl) {
        let paths = t
            .devices
            .iter()
            .map(|d| devices.get(d).cloned())
            .collect::<Option<Vec<PathBuf>>>()
            .ok_or_else(|| fail(format!("the devices of {} were not found", t.what)))?;
        let target = under(&root, &t.mount);
        let step = format!(
            "mount {} ({}) at {}",
            t.what,
            paths[0].display(),
            target.display()
        );
        let mounts = sys.mounts().map_err(fail)?;
        // The last mount on a path is the one that is seen there.
        if let Some(m) = mounts.iter().rev().find(|m| m.target == target) {
            let same_device = m.source.as_ref().is_some_and(|s| paths.contains(s));
            let same_root = match &t.subvolume {
                Some(path) => m.root == format!("/{path}"),
                None => m.root == "/",
            };
            if same_device && same_root {
                continue;
            }
            return Err(fail(format!(
                "{} is already a mount point of {}",
                target.display(),
                m.source
                    .as_ref()
                    .map_or("something else".to_owned(), |s| s.display().to_string())
            )));
        }
        if t.format == Format::Btrfs && checked.insert(t.filesystem.clone()) {
            check_btrfs_devices(sys, &t.what, &paths)?;
        }
        // The mount point may not exist yet in a filesystem mounted above.
        fs::create_dir_all(&target)
            .map_err(|e| fail(format!("cannot create {}: {e}", target.display())))?;
        act.mount(t.format, &paths, &t.options, &target)
            .map_err(|e| fail(format!("{step}: {e}")))?;
        let mounted =
            sys.mounts().map_err(fail)?.iter().any(|m| {
                m.target == target && m.source.as_ref().is_some_and(|s| paths.contains(s))
            });
        if !mounted {
            return Err(fail(format!("{step}: it is not mounted there")));
        }
        done(step);
    }

    // Now that the btrfs is mounted, the judgment reads what it could not.
    let mounted: BTreeSet<Name> = targets(decl).into_iter().map(|t| t.filesystem).collect();
    let mut issues = Vec::new();
    for j in judge_all(decl, sys, &r)? {
        issues.extend(j.issues);
        for (u, issue) in j.unread {
            // A btrfs without mount points is not mounted by design.
            let unmounted =
                matches!(&u, Unread::UnmountedBtrfs { name } if !mounted.contains(name));
            if !unmounted {
                issues.push(issue);
            }
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues)
    }
}

/// Before mounting a btrfs, checks that its devices on this machine are
/// exactly the declared ones, as far as it can be read without mounting.
fn check_btrfs_devices(sys: &dyn System, what: &str, paths: &[PathBuf]) -> Result<(), Vec<Issue>> {
    let uuid = sys
        .probe(&paths[0])
        .map_err(fail)?
        .uuid
        .ok_or_else(|| fail(format!("{what}: blkid found no btrfs UUID")))?;
    let members = sys.btrfs_members(&uuid).map_err(fail)?;
    let declared: BTreeSet<PathBuf> = paths.iter().cloned().collect();
    if members.devices != declared || members.count != declared.len() as u64 {
        return Err(fail(format!(
            "{what}: btrfs {uuid} has {} devices, {:?} of them on this machine, but the \
             declaration has {:?}",
            members.count, members.devices, declared
        )));
    }
    Ok(())
}

/// One step of unmount, in the order unmount performs it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Unmount { target: PathBuf, source: PathBuf },
    DeactivateVolumeGroup { name: Name },
    CloseLuks { name: String, device: PathBuf },
}

impl std::fmt::Display for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Step::Unmount { target, source } => {
                write!(f, "unmount {} ({})", target.display(), source.display())
            }
            Step::DeactivateVolumeGroup { name } => write!(f, "deactivate volume group {name}"),
            Step::CloseLuks { name, device } => {
                write!(f, "close LUKS mapping {name} on {}", device.display())
            }
        }
    }
}

/// ADR 0019: unmounts the filesystems of the declaration under `root`,
/// deactivates its volume groups and closes its LUKS mappings, calling
/// `done` after each. Returns whether there was anything to do.
pub fn unmount(
    decl: &Declaration,
    sys: &dyn System,
    act: &dyn Activate,
    root: &Path,
    warn: &mut dyn FnMut(String),
    done: &mut dyn FnMut(String),
) -> Result<bool, Vec<Issue>> {
    let root = root_dir(root)?;
    let r = resolve(decl, sys, warn)?;
    let declared: BTreeSet<PathBuf> = r.disks.values().map(|d| d.path.clone()).collect();
    let on_declared = |device: &Path| -> Result<bool, Vec<Issue>> {
        Ok(disks_under(sys, device).map_err(fail)?.is_subset(&declared))
    };

    let mounts = sys.mounts().map_err(fail)?;
    let targets: BTreeSet<PathBuf> = targets(decl)
        .iter()
        .map(|t| under(&root, &t.mount))
        .collect();
    let mut steps = Vec::new();
    let mut issues = Vec::new();
    // The later mounts are unmounted first, as they may be on the earlier.
    for m in mounts.iter().rev() {
        if !m.target.starts_with(&root) {
            continue;
        }
        let target = m.target.display();
        let declared_device = match &m.source {
            Some(source) => on_declared(source)?,
            None => false,
        };
        match (&m.source, targets.contains(&m.target) && declared_device) {
            (Some(source), true) => steps.push(Step::Unmount {
                target: m.target.clone(),
                source: source.clone(),
            }),
            _ => issues.push(Issue {
                at: "unmount".to_owned(),
                message: format!(
                    "{target} is mounted under {}, but not from the declaration; unmount it \
                     first",
                    root.display()
                ),
            }),
        }
    }

    // The devices that deactivating and closing remove.
    let mut removed: BTreeSet<PathBuf> = BTreeSet::new();
    let groups = sys.volume_groups().map_err(fail)?;
    for name in decl.lvm.keys() {
        let Some(group) = groups.get(name.as_str()) else {
            continue;
        };
        let active: Vec<PathBuf> = group
            .volumes
            .values()
            .filter_map(|v| v.device.clone())
            .collect();
        if active.is_empty() {
            continue;
        }
        for pv in &group.devices {
            if pv.as_os_str() == "[unknown]" || !on_declared(pv)? {
                issues.push(Issue {
                    at: format!("lvm.{name}"),
                    message: format!(
                        "the volume group `{name}` is also on {}, which is not in the \
                         declaration",
                        pv.display()
                    ),
                });
            }
        }
        removed.extend(active);
        steps.push(Step::DeactivateVolumeGroup { name: name.clone() });
    }
    for (name, luks) in &decl.luks {
        let Some(device) = existing(decl, sys, &r.disks, &luks.device).map_err(fail)? else {
            continue;
        };
        let Some(mapping) = sys.luks_mapping(&device).map_err(fail)? else {
            continue;
        };
        let node = sys.node(&mapping).map_err(fail)?;
        let NodeKind::Luks { name: mapper } = node.kind else {
            return Err(fail(format!("{} is not a LUKS mapping", mapping.display())));
        };
        for holder in &node.holders {
            if !removed.contains(holder) {
                issues.push(Issue {
                    at: format!("luks.{name}"),
                    message: format!(
                        "{} is used by {}, which is not in the declaration",
                        mapping.display(),
                        holder.display()
                    ),
                });
            }
        }
        removed.insert(mapping);
        steps.push(Step::CloseLuks {
            name: mapper,
            device,
        });
    }

    // What is removed must not be in use but by the mounts unmounted here.
    let uses = sys.uses().map_err(fail)?;
    for device in &removed {
        for m in &mounts {
            if m.source.as_ref() == Some(device) && !targets.contains(&m.target) {
                issues.push(Issue {
                    at: "unmount".to_owned(),
                    message: format!(
                        "{} is mounted at {}, which is not a mount point of the declaration \
                         under {}",
                        device.display(),
                        m.target.display(),
                        root.display()
                    ),
                });
            }
        }
        if uses.get(device).is_some_and(|u| u == "active swap") {
            issues.push(Issue {
                at: "unmount".to_owned(),
                message: format!(
                    "{} is active swap, which unmount does not turn off (ADR 0019)",
                    device.display()
                ),
            });
        }
    }
    if !issues.is_empty() {
        return Err(issues);
    }

    for step in &steps {
        perform(step, sys, act).map_err(|e| fail(format!("{step}: {e}")))?;
        done(step.to_string());
    }
    Ok(!steps.is_empty())
}

fn perform(step: &Step, sys: &dyn System, act: &dyn Activate) -> Result<(), String> {
    match step {
        Step::Unmount { target, source } => {
            // A device may be mounted on the same path more than once, and
            // each step unmounts one of them.
            let count = || -> Result<usize, String> {
                let same =
                    |m: &&MountEntry| &m.target == target && m.source.as_ref() == Some(source);
                Ok(sys.mounts()?.iter().filter(same).count())
            };
            let before = count()?;
            act.unmount(target)?;
            if count()? >= before {
                return Err("it is still mounted".to_owned());
            }
            Ok(())
        }
        Step::DeactivateVolumeGroup { name } => {
            act.deactivate_volume_group(name.as_str())?;
            let active = sys
                .volume_groups()?
                .get(name.as_str())
                .is_some_and(|g| g.volumes.values().any(|v| v.device.is_some()));
            if active {
                return Err("some of its logical volumes are still active".to_owned());
            }
            Ok(())
        }
        Step::CloseLuks { name, device } => {
            act.close_luks(name)?;
            if sys.luks_mapping(device)?.is_some() {
                return Err(format!("{} is still open", device.display()));
            }
            Ok(())
        }
    }
}

/// The existing device of a declared partition or logical volume; `None`
/// if it does not exist or is not active.
fn existing(
    decl: &Declaration,
    sys: &dyn System,
    disks: &BTreeMap<Name, BlockDevice>,
    r: &Ref,
) -> Result<Option<PathBuf>, String> {
    match r {
        Ref::Partition { disk, partition } => {
            let number = decl.disk[disk]
                .partitions
                .iter()
                .position(|p| &p.name == partition)
                .expect("a valid declaration")
                + 1;
            Ok(sys
                .partition_table(&disks[disk])?
                .into_iter()
                .flat_map(|t| t.partitions)
                .find(|e| e.number as usize == number)
                .map(|e| e.device))
        }
        Ref::Luks { name } => match existing(decl, sys, disks, &decl.luks[name].device)? {
            Some(device) => sys.luks_mapping(&device),
            None => Ok(None),
        },
        Ref::LogicalVolume { vg, lv } => Ok(sys
            .volume_groups()?
            .get(vg.as_str())
            .and_then(|g| g.volumes.get(lv.as_str()))
            .and_then(|v| v.device.clone())),
    }
}

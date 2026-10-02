//! Rules that relate several values of a declaration (ADR 0002, ADR 0005,
//! ADR 0007). Each rule reports every violation it finds, so that a single
//! run shows all of them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::model::{Declaration, Filesystem, Format, Label, Ref};
use crate::size::Size;

#[derive(Debug, PartialEq, Eq)]
pub struct Issue {
    /// Where in the declaration the problem is, such as `filesystem.data.devices[1]`.
    pub at: String,
    pub message: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.at, self.message)
    }
}

pub fn validate(decl: &Declaration) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut report = |at: String, message: String| issues.push(Issue { at, message });
    check_partitions(decl, &mut report);
    check_volume_groups(decl, &mut report);
    check_filesystems(decl, &mut report);
    check_references(decl, &mut report);
    check_labels(decl, &mut report);
    check_mount_points(decl, &mut report);
    check_mapper_names(decl, &mut report);
    issues
}

type Report<'a> = dyn FnMut(String, String) + 'a;

/// ADR 0002: percentages of one parent must not add up to more than 100%.
fn check_percent_total<'s>(at: String, sizes: impl Iterator<Item = &'s Size>, report: &mut Report) {
    let total: u32 = sizes
        .filter_map(|s| match s {
            Size::Percent(p) => Some(u32::from(*p)),
            Size::Fixed(_) => None,
        })
        .sum();
    if total > 100 {
        report(
            at,
            format!("percentages add up to {total}%, more than 100%"),
        );
    }
}

fn check_partitions(decl: &Declaration, report: &mut Report) {
    for (disk_name, disk) in &decl.disk {
        let mut seen = BTreeSet::new();
        for (i, p) in disk.partitions.iter().enumerate() {
            if !seen.insert(&p.name) {
                report(
                    format!("disk.{disk_name}.partitions[{i}].name"),
                    format!("duplicate partition name `{}`", p.name),
                );
            }
        }
        check_percent_total(
            format!("disk.{disk_name}.partitions"),
            disk.partitions.iter().map(|p| &p.size),
            report,
        );
    }
}

fn check_volume_groups(decl: &Declaration, report: &mut Report) {
    for (vg_name, vg) in &decl.lvm {
        if vg.devices.is_empty() {
            report(
                format!("lvm.{vg_name}.devices"),
                "must not be empty".to_owned(),
            );
        }
        check_percent_total(
            format!("lvm.{vg_name}.volumes"),
            vg.volumes.values().map(|lv| &lv.size),
            report,
        );
    }
}

/// The devices a filesystem is declared on, whichever key was used.
fn filesystem_devices(fs: &Filesystem) -> &[Ref] {
    match (&fs.device, &fs.devices) {
        (Some(device), _) => std::slice::from_ref(device),
        (None, Some(devices)) => devices,
        (None, None) => &[],
    }
}

fn check_filesystems(decl: &Declaration, report: &mut Report) {
    for (name, fs) in &decl.filesystem {
        let at = |key: &str| format!("filesystem.{name}.{key}");
        let btrfs = fs.format == Format::Btrfs;
        match (&fs.device, &fs.devices) {
            (Some(_), Some(_)) => report(
                at("devices"),
                "`device` and `devices` are exclusive".to_owned(),
            ),
            (None, None) => report(
                format!("filesystem.{name}"),
                "either `device` or `devices` is required".to_owned(),
            ),
            (None, Some(_)) if !btrfs => report(
                at("devices"),
                format!(
                    "`devices` is accepted only for btrfs, not {}; use `device`",
                    fs.format
                ),
            ),
            (None, Some(devices)) if devices.is_empty() => {
                report(at("devices"), "must not be empty".to_owned())
            }
            _ => {}
        }
        if !btrfs {
            for key in [
                fs.btrfs.is_some().then_some("btrfs"),
                fs.subvolumes.is_some().then_some("subvolumes"),
            ]
            .into_iter()
            .flatten()
            {
                report(
                    at(key),
                    format!("`{key}` is accepted only for btrfs, not {}", fs.format),
                );
            }
        }
        if let Some(options) = &fs.btrfs {
            let count = filesystem_devices(fs).len();
            for (key, profile) in [
                ("data_profile", options.data_profile),
                ("metadata_profile", options.metadata_profile),
            ] {
                if count < profile.min_devices() {
                    report(
                        format!("filesystem.{name}.btrfs.{key}"),
                        format!(
                            "{profile} needs at least {} devices, but {count} declared",
                            profile.min_devices()
                        ),
                    );
                }
            }
        }
        if let Some(subvolumes) = &fs.subvolumes {
            let mut seen = BTreeMap::new();
            for (sub_name, sub) in subvolumes {
                if let Some(other) = seen.insert(&sub.path, sub_name) {
                    report(
                        format!("filesystem.{name}.subvolumes.{sub_name}.path"),
                        format!(
                            "subvolume path `{}` is also used by `{other}`",
                            sub.path.as_str()
                        ),
                    );
                }
            }
        }
        for (i, option) in fs.mount_options.iter().flatten().enumerate() {
            if option.is_empty() {
                report(
                    at(&format!("mount_options[{i}]")),
                    "must not be empty".to_owned(),
                );
            }
        }
    }
}

/// Every place where a device is referenced, with its location.
fn consumers(decl: &Declaration) -> Vec<(String, &Ref)> {
    let mut list = Vec::new();
    for (name, luks) in &decl.luks {
        list.push((format!("luks.{name}.device"), &luks.device));
    }
    for (name, vg) in &decl.lvm {
        for (i, r) in vg.devices.iter().enumerate() {
            list.push((format!("lvm.{name}.devices[{i}]"), r));
        }
    }
    for (name, fs) in &decl.filesystem {
        if let Some(r) = &fs.device {
            list.push((format!("filesystem.{name}.device"), r));
        }
        for (i, r) in fs.devices.iter().flatten().enumerate() {
            list.push((format!("filesystem.{name}.devices[{i}]"), r));
        }
    }
    for (name, swap) in &decl.swap {
        list.push((format!("swap.{name}.device"), &swap.device));
    }
    list
}

fn exists(decl: &Declaration, r: &Ref) -> bool {
    match r {
        Ref::Partition { disk, partition } => decl
            .disk
            .get(disk)
            .is_some_and(|d| d.partitions.iter().any(|p| &p.name == partition)),
        Ref::Luks { name } => decl.luks.contains_key(name),
        Ref::LogicalVolume { vg, lv } => {
            decl.lvm.get(vg).is_some_and(|g| g.volumes.contains_key(lv))
        }
    }
}

/// The devices that a device is built on.
fn dependencies<'d>(decl: &'d Declaration, r: &Ref) -> &'d [Ref] {
    match r {
        Ref::Partition { .. } => &[],
        Ref::Luks { name } => decl
            .luks
            .get(name)
            .map_or(&[], |l| std::slice::from_ref(&l.device)),
        Ref::LogicalVolume { vg, .. } => decl.lvm.get(vg).map_or(&[], |g| &g.devices),
    }
}

/// ADR 0005: references must resolve, a device has at most one user, and
/// references must not form a cycle.
fn check_references(decl: &Declaration, report: &mut Report) {
    let mut users: BTreeMap<&Ref, String> = BTreeMap::new();
    for (at, r) in consumers(decl) {
        if !exists(decl, r) {
            report(at, format!("`{r}` is not declared"));
        } else if let Some(first) = users.get(r) {
            report(at, format!("`{r}` is already used by {first}"));
        } else {
            users.insert(r, at);
        }
    }

    // Depth-first search over the devices that have dependencies.
    let mut done = BTreeSet::new();
    let roots = decl
        .luks
        .keys()
        .map(|name| Ref::Luks { name: name.clone() })
        .chain(decl.lvm.iter().flat_map(|(vg, g)| {
            g.volumes.keys().map(|lv| Ref::LogicalVolume {
                vg: vg.clone(),
                lv: lv.clone(),
            })
        }));
    for root in roots {
        let mut stack = Vec::new();
        find_cycles(decl, root, &mut stack, &mut done, report);
    }
}

fn find_cycles(
    decl: &Declaration,
    node: Ref,
    stack: &mut Vec<Ref>,
    done: &mut BTreeSet<Ref>,
    report: &mut Report,
) {
    if done.contains(&node) {
        return;
    }
    if let Some(start) = stack.iter().position(|r| r == &node) {
        let cycle: Vec<String> = stack[start..]
            .iter()
            .chain([&node])
            .map(Ref::to_string)
            .collect();
        report(
            node.to_string(),
            format!("reference cycle: {}", cycle.join(" -> ")),
        );
        return;
    }
    stack.push(node.clone());
    for dep in dependencies(decl, &node) {
        if exists(decl, dep) {
            find_cycles(decl, dep.clone(), stack, done, report);
        }
    }
    stack.pop();
    done.insert(node);
}

/// The longest label in bytes that each filesystem format can store.
fn label_limit(format: Format) -> usize {
    match format {
        Format::Vfat => 11,
        Format::Ext4 => 16,
        Format::Xfs => 12,
        Format::Btrfs => 255,
    }
}

const SWAP_LABEL_LIMIT: usize = 16;
/// A GPT partition name holds 36 UTF-16 code units.
const PARTLABEL_LIMIT: usize = 36;

/// ADR 0005: label length limits and uniqueness.
fn check_labels(decl: &Declaration, report: &mut Report) {
    let mut partlabels: BTreeMap<&str, String> = BTreeMap::new();
    for (disk_name, disk) in &decl.disk {
        for (i, p) in disk.partitions.iter().enumerate() {
            let Some(label) = &p.label else { continue };
            let at = format!("disk.{disk_name}.partitions[{i}].label");
            let units = label.as_str().encode_utf16().count();
            if units > PARTLABEL_LIMIT {
                report(
                    at.clone(),
                    format!(
                        "partition label `{label}` is {units} UTF-16 code units, more than {PARTLABEL_LIMIT}"
                    ),
                );
            }
            unique(&mut partlabels, label, at, report);
        }
    }

    // Filesystem and swap labels share /dev/disk/by-label/.
    let mut labels: BTreeMap<&str, String> = BTreeMap::new();
    for (name, fs) in &decl.filesystem {
        if let Some(label) = &fs.label {
            let at = format!("filesystem.{name}.label");
            check_length(label, label_limit(fs.format), fs.format, &at, report);
            unique(&mut labels, label, at, report);
        }
    }
    for (name, swap) in &decl.swap {
        if let Some(label) = &swap.label {
            let at = format!("swap.{name}.label");
            check_length(label, SWAP_LABEL_LIMIT, "swap", &at, report);
            unique(&mut labels, label, at, report);
        }
    }
}

fn check_length(
    label: &Label,
    limit: usize,
    kind: impl fmt::Display,
    at: &str,
    report: &mut Report,
) {
    let bytes = label.as_str().len();
    if bytes > limit {
        report(
            at.to_owned(),
            format!("{kind} label `{label}` is {bytes} bytes, more than {limit}"),
        );
    }
}

fn unique<'d>(
    seen: &mut BTreeMap<&'d str, String>,
    label: &'d Label,
    at: String,
    report: &mut Report,
) {
    if let Some(first) = seen.get(label.as_str()) {
        report(at, format!("label `{label}` is also used at {first}"));
    } else {
        seen.insert(label.as_str(), at);
    }
}

/// ADR 0007: two filesystems or subvolumes must not be mounted at the same place.
fn check_mount_points(decl: &Declaration, report: &mut Report) {
    let mut seen: BTreeMap<&str, String> = BTreeMap::new();
    for (name, fs) in &decl.filesystem {
        let subvolumes = fs.subvolumes.iter().flatten();
        let mounts = fs
            .mount
            .iter()
            .map(|m| (m, format!("filesystem.{name}.mount")))
            .chain(subvolumes.filter_map(|(sub, s)| {
                Some((
                    s.mount.as_ref()?,
                    format!("filesystem.{name}.subvolumes.{sub}.mount"),
                ))
            }));
        for (mount, at) in mounts {
            if let Some(first) = seen.get(mount.as_str()) {
                report(
                    at,
                    format!("mount point `{}` is also used at {first}", mount.as_str()),
                );
            } else {
                seen.insert(mount.as_str(), at);
            }
        }
    }
}

/// ADR 0007: LUKS mappings and logical volumes share the device-mapper
/// namespace, where a logical volume appears as `<vg>-<lv>` with each `-`
/// in the names doubled.
fn check_mapper_names(decl: &Declaration, report: &mut Report) {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let luks = decl
        .luks
        .keys()
        .map(|name| (name.to_string(), format!("luks.{name}")));
    let lvs = decl.lvm.iter().flat_map(|(vg, g)| {
        g.volumes.keys().map(move |lv| {
            let escape = |s: &str| s.replace('-', "--");
            (
                format!("{}-{}", escape(vg.as_str()), escape(lv.as_str())),
                format!("lvm.{vg}.volumes.{lv}"),
            )
        })
    });
    for (mapper, at) in luks.chain(lvs) {
        if let Some(first) = seen.get(&mapper) {
            report(
                at,
                format!("device-mapper name `{mapper}` is also used by {first}"),
            );
        } else {
            seen.insert(mapper, at);
        }
    }
}

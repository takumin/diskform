//! Absolute sizes computed from a declaration and the capacities of the
//! matched disks (ADR 0002, ADR 0009, ADR 0010, ADR 0012). Nothing here
//! reads a device.

use std::collections::BTreeMap;

use crate::model::{Declaration, Name, Ref};
use crate::size::Size;
use crate::validate::Issue;

pub const MIB: u64 = 1 << 20;
/// Partitions are aligned to 1MiB (ADR 0002).
pub const PARTITION_ALIGNMENT: u64 = MIB;
/// Where the data of a LUKS device starts (ADR 0009).
pub const LUKS_DATA_OFFSET: u64 = 16 * MIB;
/// Where the extents of a physical volume start (ADR 0010).
pub const PV_DATA_OFFSET: u64 = MIB;
/// The physical extent size of every volume group (ADR 0010).
pub const EXTENT_SIZE: u64 = 4 * MIB;
/// The size of the GPT partition entry array: 128 entries of 128 bytes.
const GPT_ENTRIES: u64 = 128 * 128;

/// What the layout needs to know about a matched disk.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub size: u64,
    pub logical_sector: u64,
}

/// The region of a GPT disk where partitions are placed (ADR 0012): the
/// usable LBAs between the primary and backup tables, shrunk to 1MiB
/// boundaries. `None` if the disk is too small to hold any of it.
pub fn gpt_area(g: Geometry) -> Option<(u64, u64)> {
    let ss = g.logical_sector;
    let sectors = g.size / ss;
    let entry_sectors = GPT_ENTRIES.div_ceil(ss);
    // LBA 0 is the protective MBR and LBA 1 the primary header; the backup
    // header is the last LBA, preceded by the backup entries.
    let first_usable = (2 + entry_sectors) * ss;
    let last_usable = sectors.checked_sub(2 + entry_sectors)?;
    let start = first_usable.next_multiple_of(PARTITION_ALIGNMENT);
    let end = (last_usable + 1) * ss / PARTITION_ALIGNMENT * PARTITION_ALIGNMENT;
    (end > start).then_some((start, end))
}

/// Divides `capacity` among `sizes` (ADR 0002): fixed sizes are rounded up
/// to `unit`, and percentages of what the fixed sizes leave are rounded down
/// to `unit`.
pub fn allocate(capacity: u64, unit: u64, sizes: &[&Size]) -> Result<Vec<u64>, String> {
    let fixed = |bytes: u64| bytes.div_ceil(unit) * unit;
    let fixed_total = sizes
        .iter()
        .filter_map(|s| match s {
            Size::Fixed(bytes) => Some(fixed(*bytes)),
            Size::Percent(_) => None,
        })
        .try_fold(0u64, u64::checked_add)
        .filter(|total| *total <= capacity)
        .ok_or_else(|| {
            format!(
                "fixed sizes do not fit in the capacity of {}",
                format_size(capacity)
            )
        })?;
    let base = capacity - fixed_total;
    sizes
        .iter()
        .map(|s| match s {
            Size::Fixed(bytes) => Ok(fixed(*bytes)),
            Size::Percent(p) => {
                let bytes = (u128::from(base) * u128::from(*p) / 100) as u64 / unit * unit;
                if bytes == 0 {
                    Err(format!(
                        "{p}% of the {} left by fixed sizes is less than {}",
                        format_size(base),
                        format_size(unit)
                    ))
                } else {
                    Ok(bytes)
                }
            }
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub start: u64,
    pub size: u64,
}

#[derive(Debug)]
pub struct VolumeGroupLayout {
    pub capacity: u64,
    pub volumes: BTreeMap<Name, u64>,
}

#[derive(Debug, Default)]
pub struct Layout {
    /// The partitions of each disk, in declaration order.
    pub partitions: BTreeMap<Name, Vec<Extent>>,
    /// The size of each opened LUKS device.
    pub luks: BTreeMap<Name, u64>,
    pub volume_groups: BTreeMap<Name, VolumeGroupLayout>,
}

impl Layout {
    /// The size of a declared block device, if it was computed.
    pub fn device_size(&self, decl: &Declaration, r: &Ref) -> Option<u64> {
        match r {
            Ref::Partition { disk, partition } => {
                let i = decl.disk[disk]
                    .partitions
                    .iter()
                    .position(|p| &p.name == partition)?;
                Some(self.partitions.get(disk)?[i].size)
            }
            Ref::Luks { name } => self.luks.get(name).copied(),
            Ref::LogicalVolume { vg, lv } => self.volume_groups.get(vg)?.volumes.get(lv).copied(),
        }
    }
}

/// The capacity of a physical volume on a device of `size` bytes.
pub fn pv_capacity(size: u64) -> u64 {
    size.saturating_sub(PV_DATA_OFFSET) / EXTENT_SIZE * EXTENT_SIZE
}

/// Computes every size in a valid declaration from the geometries of its
/// disks. An element whose size depends on a failed element is left out
/// without a further issue.
pub fn compute(decl: &Declaration, disks: &BTreeMap<Name, Geometry>) -> (Layout, Vec<Issue>) {
    let mut layout = Layout::default();
    let mut issues = Vec::new();
    for (name, disk) in &decl.disk {
        let at = format!("disk.{name}.partitions");
        let Some((start, end)) = gpt_area(disks[name]) else {
            issues.push(Issue {
                at,
                message: "the disk is too small for a GPT".to_owned(),
            });
            continue;
        };
        let sizes: Vec<&Size> = disk.partitions.iter().map(|p| &p.size).collect();
        match allocate(end - start, PARTITION_ALIGNMENT, &sizes) {
            Ok(sizes) => {
                let mut next = start;
                let extents = sizes
                    .into_iter()
                    .map(|size| {
                        let e = Extent { start: next, size };
                        next += size;
                        e
                    })
                    .collect();
                layout.partitions.insert(name.clone(), extents);
            }
            Err(message) => issues.push(Issue { at, message }),
        }
    }

    // LUKS devices and volume groups may stack on each other in any order,
    // so resolve them in rounds until nothing more can be computed.
    let mut too_small = std::collections::BTreeSet::new();
    loop {
        let mut progress = false;
        for (name, luks) in &decl.luks {
            if layout.luks.contains_key(name) || too_small.contains(name) {
                continue;
            }
            let Some(size) = layout.device_size(decl, &luks.device) else {
                continue;
            };
            progress = true;
            if size <= LUKS_DATA_OFFSET {
                issues.push(Issue {
                    at: format!("luks.{name}.device"),
                    message: format!(
                        "`{}` is {}, too small for a LUKS header of {}",
                        luks.device,
                        format_size(size),
                        format_size(LUKS_DATA_OFFSET)
                    ),
                });
                too_small.insert(name);
            } else {
                layout.luks.insert(name.clone(), size - LUKS_DATA_OFFSET);
            }
        }
        for (name, vg) in &decl.lvm {
            if layout.volume_groups.contains_key(name) {
                continue;
            }
            let Some(sizes) = vg
                .devices
                .iter()
                .map(|r| layout.device_size(decl, r))
                .collect::<Option<Vec<u64>>>()
            else {
                continue;
            };
            progress = true;
            let capacity = sizes.into_iter().map(pv_capacity).sum();
            let names: Vec<&Name> = vg.volumes.keys().collect();
            let sizes: Vec<&Size> = vg.volumes.values().map(|lv| &lv.size).collect();
            let volumes = match allocate(capacity, EXTENT_SIZE, &sizes) {
                Ok(sizes) => names.into_iter().cloned().zip(sizes).collect(),
                Err(message) => {
                    issues.push(Issue {
                        at: format!("lvm.{name}.volumes"),
                        message,
                    });
                    BTreeMap::new()
                }
            };
            layout
                .volume_groups
                .insert(name.clone(), VolumeGroupLayout { capacity, volumes });
        }
        if !progress {
            break;
        }
    }
    (layout, issues)
}

/// Formats a byte count in the largest unit from MiB up that divides it,
/// or else approximately in GiB with the exact byte count.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [(&str, u32); 3] = [("TiB", 40), ("GiB", 30), ("MiB", 20)];
    for (unit, shift) in UNITS {
        if bytes != 0 && bytes % (1 << shift) == 0 {
            return format!("{} {unit}", bytes >> shift);
        }
    }
    if bytes < 1 << 30 {
        return format!("{bytes} bytes");
    }
    format!(
        "{:.1} GiB ({bytes} bytes)",
        bytes as f64 / f64::from(1u32 << 30)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn gpt_area_leaves_room_for_both_tables() {
        // 512-byte sectors: 34 sectors at the start and 33 at the end.
        let g = Geometry {
            size: 10 * GIB,
            logical_sector: 512,
        };
        assert_eq!(gpt_area(g), Some((MIB, 10 * GIB - MIB)));
        // 4096-byte sectors: 6 sectors at the start and 5 at the end.
        let g = Geometry {
            size: 10 * GIB,
            logical_sector: 4096,
        };
        assert_eq!(gpt_area(g), Some((MIB, 10 * GIB - MIB)));
        // A size that is not a multiple of 1MiB loses the remainder too.
        let g = Geometry {
            size: 10 * GIB + 1000 * 512,
            logical_sector: 512,
        };
        assert_eq!(gpt_area(g), Some((MIB, 10 * GIB)));
        let g = Geometry {
            size: 2 * MIB,
            logical_sector: 512,
        };
        assert_eq!(gpt_area(g), None);
    }

    #[test]
    fn allocate_follows_adr_0002() {
        let fixed = Size::Fixed(GIB);
        let half = Size::Percent(50);
        assert_eq!(
            allocate(11 * GIB, MIB, &[&fixed, &half, &half]),
            Ok(vec![GIB, 5 * GIB, 5 * GIB])
        );
        // Fixed sizes round up, percentages round down.
        let odd = Size::Fixed(MIB + 1);
        let third = Size::Percent(33);
        assert_eq!(
            allocate(100 * MIB + MIB + 1, MIB, &[&odd, &third]),
            Ok(vec![2 * MIB, 32 * MIB])
        );
        assert!(allocate(GIB - MIB, MIB, &[&fixed]).is_err());
        assert!(allocate(GIB, MIB, &[&fixed, &half]).is_err());
        let one = Size::Percent(1);
        assert!(allocate(50 * MIB, MIB, &[&one]).is_err());
    }

    #[test]
    fn pv_capacity_follows_adr_0010() {
        assert_eq!(pv_capacity(9 * MIB), 8 * MIB);
        assert_eq!(pv_capacity(8 * MIB), 4 * MIB);
        assert_eq!(pv_capacity(MIB), 0);
    }

    #[test]
    fn sizes_are_formatted_exactly_when_possible() {
        assert_eq!(format_size(GIB), "1 GiB");
        assert_eq!(format_size(1536 * MIB), "1536 MiB");
        assert_eq!(format_size(4 << 40), "4 TiB");
        assert_eq!(format_size(512), "512 bytes");
        assert_eq!(format_size(GIB + 512), "1.0 GiB (1073742336 bytes)");
    }
}

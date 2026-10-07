//! A fake machine for the plan-layer tests. The real machine is read only
//! through `System`, so the tests need neither privileges nor devices.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use diskform::layout::MIB;
use diskform::system::{
    BlockDevice, BtrfsState, DeviceKind, DiskState, LogicalVolumeState, PartitionEntry,
    PartitionTable, Probe, System, VolumeGroupState,
};

pub const GIB: u64 = 1 << 30;
pub const TIB: u64 = 1 << 40;

#[derive(Default)]
pub struct Fake {
    /// Symbolic links and their canonical targets.
    pub links: BTreeMap<PathBuf, PathBuf>,
    pub devices: BTreeMap<PathBuf, BlockDevice>,
    pub states: BTreeMap<PathBuf, DiskState>,
    pub keyfiles: Vec<PathBuf>,
    pub tables: BTreeMap<PathBuf, PartitionTable>,
    pub probes: BTreeMap<PathBuf, Probe>,
    /// LUKS devices and the key files that open them.
    pub keys: BTreeMap<PathBuf, PathBuf>,
    /// Open LUKS devices and their mappings.
    pub mappings: BTreeMap<PathBuf, PathBuf>,
    pub volume_groups: BTreeMap<String, VolumeGroupState>,
    /// Mounted btrfs filesystems by UUID.
    pub btrfs: BTreeMap<String, BtrfsState>,
    pub mappers: Vec<String>,
}

pub const LINUX: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
pub const ESP: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
pub const DATA_UUID: &str = "5d1c7e2a-3b4f-4c6d-8e9f-0a1b2c3d4e5f";

pub fn probe(kind: &str, label: &str) -> Probe {
    Probe {
        kind: Some(kind.to_owned()),
        label: (!label.is_empty()).then(|| label.to_owned()),
        ..Probe::default()
    }
}

pub fn paths<const N: usize>(paths: [&str; N]) -> BTreeSet<PathBuf> {
    paths.into_iter().map(PathBuf::from).collect()
}

impl Fake {
    pub fn device(mut self, path: &str, kind: DeviceKind, size: u64) -> Self {
        let device = BlockDevice {
            path: path.into(),
            kind,
            size,
            logical_sector: 512,
            model: Some("Model".to_owned()),
            serial: Some(format!("SN-{}", path.rsplit('/').next().unwrap())),
        };
        self.devices.insert(path.into(), device);
        self
    }

    pub fn disk(self, path: &str, size: u64) -> Self {
        self.device(path, DeviceKind::Disk, size)
    }

    pub fn link(mut self, link: &str, target: &str) -> Self {
        self.links.insert(link.into(), target.into());
        self
    }

    pub fn state(mut self, path: &str, state: DiskState) -> Self {
        self.states.insert(path.into(), state);
        self
    }

    pub fn keyfile(mut self, path: &str) -> Self {
        self.keyfiles.push(path.into());
        self
    }

    /// A machine that the example declaration matches.
    pub fn example() -> Self {
        Fake::default()
            .disk("/dev/nvme0n1", 1_000_204_886_016)
            .device("/dev/nvme0n1p1", DeviceKind::Partition, GIB)
            .link(
                "/dev/disk/by-id/nvme-Samsung_SSD_980_PRO_1TB_S5GXNX0R123456",
                "/dev/nvme0n1",
            )
            .link(
                "/dev/disk/by-id/nvme-Samsung_SSD_980_PRO_1TB_S5GXNX0R123456-part1",
                "/dev/nvme0n1p1",
            )
            .disk("/dev/sda", 4_000_787_030_016)
            .disk("/dev/sdb", 4_000_787_030_016)
            .link("/dev/disk/by-path/pci-0000:00:17.0-ata-1", "/dev/sda")
            .link("/dev/disk/by-path/pci-0000:00:17.0-ata-2", "/dev/sdb")
            .keyfile("/run/keys/sys.key")
    }

    /// A partition table and its partitions, given as (start, size, type
    /// GUID, name) in MiB.
    pub fn table(mut self, disk: &str, entries: &[(u64, u64, &str, &str)]) -> Self {
        let part = |n: usize| {
            let sep = if disk.ends_with(|c: char| c.is_ascii_digit()) {
                "p"
            } else {
                ""
            };
            format!("{disk}{sep}{n}")
        };
        let partitions = entries
            .iter()
            .enumerate()
            .map(|(i, (start, size, guid, name))| PartitionEntry {
                number: i as u32 + 1,
                start: start * MIB,
                size: size * MIB,
                type_guid: (*guid).to_owned(),
                name: (*name).to_owned(),
                device: part(i + 1).into(),
            })
            .collect();
        self.tables.insert(
            disk.into(),
            PartitionTable {
                kind: "gpt".to_owned(),
                partitions,
            },
        );
        let state = DiskState {
            signatures: vec!["gpt partition table".to_owned()],
            partitions: (1..=entries.len()).map(part).collect(),
            holders: Vec::new(),
        };
        let probe = Probe {
            table: Some("gpt".to_owned()),
            ..Probe::default()
        };
        self.probes.insert(disk.into(), probe);
        self.state(disk, state)
    }

    pub fn probe(mut self, device: &str, probe: Probe) -> Self {
        self.probes.insert(device.into(), probe);
        self
    }

    /// A machine where the example declaration has been applied, with the
    /// LUKS device open, the logical volumes active and btrfs mounted.
    pub fn configured() -> Self {
        let crypt = 951_820;
        let vg = 951_800 * MIB;
        let mut sys = Fake::example()
            .table(
                "/dev/nvme0n1",
                &[
                    (1, 1024, ESP, "esp"),
                    (1025, 1024, LINUX, "boot"),
                    (2049, crypt, LINUX, "cryptsys"),
                ],
            )
            .table("/dev/sda", &[(1, 3_815_446, LINUX, "data0")])
            .table("/dev/sdb", &[(1, 3_815_446, LINUX, "data1")])
            .probe("/dev/nvme0n1p1", probe("vfat", "EFI"))
            .probe("/dev/nvme0n1p2", probe("ext4", "boot"))
            .probe(
                "/dev/nvme0n1p3",
                Probe {
                    version: Some("2".to_owned()),
                    ..probe("crypto_LUKS", "")
                },
            )
            .probe("/dev/dm-1", probe("ext4", "root"))
            .probe("/dev/dm-2", probe("swap", "swap"))
            .probe("/dev/dm-3", probe("xfs", "var"));
        for disk in ["/dev/sda1", "/dev/sdb1"] {
            let p = Probe {
                uuid: Some(DATA_UUID.to_owned()),
                ..probe("btrfs", "data")
            };
            sys = sys.probe(disk, p);
        }
        sys.keys
            .insert("/dev/nvme0n1p3".into(), "/run/keys/sys.key".into());
        sys.mappings
            .insert("/dev/nvme0n1p3".into(), "/dev/dm-0".into());
        let lv = |size: u64, device: &str| LogicalVolumeState {
            size,
            device: Some(device.into()),
        };
        sys.volume_groups.insert(
            "vg0".to_owned(),
            VolumeGroupState {
                extent_size: 4 * MIB,
                devices: paths(["/dev/dm-0"]),
                volumes: BTreeMap::from([
                    ("root".to_owned(), lv(64 * GIB, "/dev/dm-1")),
                    ("swap".to_owned(), lv(16 * GIB, "/dev/dm-2")),
                    ("var".to_owned(), lv(vg - 80 * GIB, "/dev/dm-3")),
                ]),
            },
        );
        sys.btrfs.insert(
            DATA_UUID.to_owned(),
            BtrfsState {
                devices: paths(["/dev/sda1", "/dev/sdb1"]),
                data_profiles: BTreeSet::from(["raid1".to_owned()]),
                metadata_profiles: BTreeSet::from(["raid1".to_owned()]),
                // A snapshot that is not declared is allowed (ADR 0011).
                subvolumes: ["@srv", "@snapshots", "@snapshots/1/snapshot"]
                    .map(str::to_owned)
                    .into(),
            },
        );
        sys
    }

    pub fn vg0(&mut self) -> &mut VolumeGroupState {
        self.volume_groups.get_mut("vg0").unwrap()
    }

    pub fn data(&mut self) -> &mut BtrfsState {
        self.btrfs.get_mut(DATA_UUID).unwrap()
    }

    pub fn partition(&mut self, disk: &str, number: usize) -> &mut PartitionEntry {
        &mut self.tables.get_mut(Path::new(disk)).unwrap().partitions[number - 1]
    }
}

impl System for Fake {
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
        Ok(self
            .links
            .keys()
            .chain(self.devices.keys())
            .filter(|p| p.parent() == Some(dir))
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect())
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        if let Some(target) = self.links.get(path) {
            return Ok(target.clone());
        }
        if self.devices.contains_key(path) {
            return Ok(path.to_owned());
        }
        Err(io::ErrorKind::NotFound.into())
    }

    fn block_device(&self, path: &Path) -> io::Result<Option<BlockDevice>> {
        Ok(self.devices.get(path).cloned())
    }

    fn disk_state(&self, disk: &BlockDevice) -> Result<DiskState, String> {
        Ok(self.states.get(&disk.path).cloned().unwrap_or_default())
    }

    fn check_keyfile(&self, path: &Path) -> Result<(), String> {
        if self.keyfiles.iter().any(|k| k == path) {
            Ok(())
        } else {
            Err("does not exist".to_owned())
        }
    }

    fn partition_table(&self, disk: &BlockDevice) -> Result<Option<PartitionTable>, String> {
        Ok(self.tables.get(&disk.path).cloned())
    }

    fn probe(&self, device: &Path) -> Result<Probe, String> {
        Ok(self.probes.get(device).cloned().unwrap_or_default())
    }

    fn test_keyfile(&self, device: &Path, keyfile: &Path) -> Result<bool, String> {
        Ok(self.keys.get(device).is_some_and(|k| k == keyfile))
    }

    fn luks_mapping(&self, device: &Path) -> Result<Option<PathBuf>, String> {
        Ok(self.mappings.get(device).cloned())
    }

    fn volume_groups(&self) -> Result<BTreeMap<String, VolumeGroupState>, String> {
        Ok(self.volume_groups.clone())
    }

    fn btrfs(&self, uuid: &str) -> Result<Option<BtrfsState>, String> {
        Ok(self.btrfs.get(uuid).cloned())
    }

    fn mapper_names(&self) -> Result<Vec<String>, String> {
        Ok(self.mappers.clone())
    }
}

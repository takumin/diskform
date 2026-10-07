//! `plan` against a fake machine. The real machine is read only through
//! `System`, so these tests need neither privileges nor devices.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use diskform::layout::MIB;
use diskform::load;
use diskform::model::Name;
use diskform::plan::{Plan, plan};
use diskform::system::{
    BlockDevice, BtrfsState, DeviceKind, DiskState, LogicalVolumeState, PartitionEntry,
    PartitionTable, Probe, System, VolumeGroupState,
};

use crate::{examples, write};

const GIB: u64 = 1 << 30;
const TIB: u64 = 1 << 40;

#[derive(Default)]
struct Fake {
    /// Symbolic links and their canonical targets.
    links: BTreeMap<PathBuf, PathBuf>,
    devices: BTreeMap<PathBuf, BlockDevice>,
    states: BTreeMap<PathBuf, DiskState>,
    keyfiles: Vec<PathBuf>,
    tables: BTreeMap<PathBuf, PartitionTable>,
    probes: BTreeMap<PathBuf, Probe>,
    /// LUKS devices and the key files that open them.
    keys: BTreeMap<PathBuf, PathBuf>,
    /// Open LUKS devices and their mappings.
    mappings: BTreeMap<PathBuf, PathBuf>,
    volume_groups: BTreeMap<String, VolumeGroupState>,
    /// Mounted btrfs filesystems by UUID.
    btrfs: BTreeMap<String, BtrfsState>,
    mappers: Vec<String>,
}

const LINUX: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
const ESP: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
const DATA_UUID: &str = "5d1c7e2a-3b4f-4c6d-8e9f-0a1b2c3d4e5f";

fn probe(kind: &str, label: &str) -> Probe {
    Probe {
        kind: Some(kind.to_owned()),
        label: (!label.is_empty()).then(|| label.to_owned()),
        ..Probe::default()
    }
}

fn paths<const N: usize>(paths: [&str; N]) -> BTreeSet<PathBuf> {
    paths.into_iter().map(PathBuf::from).collect()
}

impl Fake {
    fn device(mut self, path: &str, kind: DeviceKind, size: u64) -> Self {
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

    fn disk(self, path: &str, size: u64) -> Self {
        self.device(path, DeviceKind::Disk, size)
    }

    fn link(mut self, link: &str, target: &str) -> Self {
        self.links.insert(link.into(), target.into());
        self
    }

    fn state(mut self, path: &str, state: DiskState) -> Self {
        self.states.insert(path.into(), state);
        self
    }

    fn keyfile(mut self, path: &str) -> Self {
        self.keyfiles.push(path.into());
        self
    }

    /// A machine that the example declaration matches.
    fn example() -> Self {
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
    fn table(mut self, disk: &str, entries: &[(u64, u64, &str, &str)]) -> Self {
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

    fn probe(mut self, device: &str, probe: Probe) -> Self {
        self.probes.insert(device.into(), probe);
        self
    }

    /// A machine where the example declaration has been applied, with the
    /// LUKS device open, the logical volumes active and btrfs mounted.
    fn configured() -> Self {
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

    fn vg0(&mut self) -> &mut VolumeGroupState {
        self.volume_groups.get_mut("vg0").unwrap()
    }

    fn data(&mut self) -> &mut BtrfsState {
        self.btrfs.get_mut(DATA_UUID).unwrap()
    }

    fn partition(&mut self, disk: &str, number: usize) -> &mut PartitionEntry {
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

fn example_text() -> String {
    std::fs::read_to_string(&examples()[0]).unwrap()
}

fn plan_text(text: &str, sys: &Fake) -> Plan {
    let decl = load::load(&write("yaml", text)).unwrap();
    assert!(diskform::validate::validate(&decl).is_empty());
    plan(&decl, sys)
}

#[track_caller]
fn assert_issue(plan: &Plan, at: &str, message: &str) {
    assert!(
        plan.issues
            .iter()
            .any(|i| i.at == at && i.message.contains(message)),
        "missing {at}: {message:?} in {:#?}",
        plan.issues
    );
}

fn operations(plan: &Plan) -> Vec<String> {
    plan.operations.iter().map(ToString::to_string).collect()
}

#[test]
fn example_on_empty_disks() {
    let plan = plan_text(&example_text(), &Fake::example());
    assert!(plan.issues.is_empty(), "{:#?}", plan.issues);
    let sys0 = Name::try_from("sys0".to_owned()).unwrap();
    assert_eq!(plan.disks[&sys0].path, Path::new("/dev/nvme0n1"));

    // ADR 0012: partitions fill the 1MiB-aligned area between the GPT
    // tables. ADR 0009, ADR 0010: the volume group gets what the LUKS
    // header and the PV data offset leave, in 4MiB extents.
    let crypt = 951_820 * MIB;
    let vg = 951_800 * MIB;
    assert_eq!(vg, (crypt - 16 * MIB - MIB) / (4 * MIB) * (4 * MIB));
    assert_eq!(
        operations(&plan),
        [
            "create gpt partition table on disk.data0 (/dev/sda)".to_owned(),
            "create partition 1 disk.data0.data0: 3815446 MiB at 1 MiB, type linux, label \"data0\""
                .to_owned(),
            "create gpt partition table on disk.data1 (/dev/sdb)".to_owned(),
            "create partition 1 disk.data1.data1: 3815446 MiB at 1 MiB, type linux, label \"data1\""
                .to_owned(),
            "create gpt partition table on disk.sys0 (/dev/nvme0n1)".to_owned(),
            "create partition 1 disk.sys0.esp: 1 GiB at 1 MiB, type esp, label \"esp\"".to_owned(),
            "create partition 2 disk.sys0.boot: 1 GiB at 1025 MiB, type linux, label \"boot\""
                .to_owned(),
            format!(
                "create partition 3 disk.sys0.crypt: {} MiB at 2049 MiB, type linux, label \"cryptsys\"",
                crypt / MIB
            ),
            "create LUKS2 luks.cryptsys on disk.sys0.crypt".to_owned(),
            "create physical volume on luks.cryptsys".to_owned(),
            "create volume group vg0 on luks.cryptsys".to_owned(),
            "create logical volume lvm.vg0.root: 64 GiB".to_owned(),
            "create logical volume lvm.vg0.swap: 16 GiB".to_owned(),
            format!(
                "create logical volume lvm.vg0.var: {} MiB",
                (vg - 80 * GIB) / MIB
            ),
            "create ext4 filesystem.boot on disk.sys0.boot, label \"boot\"".to_owned(),
            "create btrfs filesystem.data on disk.data0.data0, disk.data1.data1, label \"data\", data raid1, metadata raid1".to_owned(),
            "create subvolume @snapshots in filesystem.data".to_owned(),
            "create subvolume @srv in filesystem.data".to_owned(),
            "create vfat filesystem.efi on disk.sys0.esp, label \"EFI\"".to_owned(),
            "create ext4 filesystem.root on lvm.vg0.root, label \"root\"".to_owned(),
            "create xfs filesystem.var on lvm.vg0.var, label \"var\"".to_owned(),
            "create swap.main on lvm.vg0.swap, label \"swap\"".to_owned(),
        ]
    );
    assert!(plan.unused.is_empty());
    assert!(plan.warnings.is_empty());
    let shown = plan.to_string();
    assert!(shown.contains("disk.sys0: /dev/nvme0n1, model Model, serial SN-nvme0n1, "));
    assert!(shown.contains(&format!("volume group vg0: capacity {} MiB", vg / MIB)));
}

/// A declaration with one disk whose match path is `path`.
fn one_disk(path: &str, min_size: Option<&str>) -> String {
    let min = min_size.map_or(String::new(), |m| format!(", min_size: {m}"));
    format!(
        "version: 1\ndisk:\n  d0:\n    match: {{path: '{path}'{min}}}\n    table: gpt\n    partitions:\n    - {{name: p1, size: 100%, label: ''}}\n"
    )
}

#[test]
fn disks_are_matched_as_adr_0003_says() {
    let sys = Fake::example();
    let by_id = "/dev/disk/by-id/nvme-Samsung_SSD_980_PRO_1TB_*";

    // The wildcard also matches the `-part1` link, which resolves to a
    // partition and is therefore not a candidate.
    let plan = plan_text(&one_disk(by_id, None), &sys);
    assert!(plan.issues.is_empty(), "{:#?}", plan.issues);

    let plan = plan_text(&one_disk("/dev/disk/by-id/ata-*", None), &sys);
    assert_issue(&plan, "disk.d0.match", "no device matches");

    let plan = plan_text(&one_disk("/dev/disk/by-id/*-part1", None), &sys);
    assert_issue(
        &plan,
        "disk.d0.match",
        "matches no whole disk, only /dev/nvme0n1p1 (partition)",
    );

    let plan = plan_text(&one_disk("/dev/sd?", None), &sys);
    assert_issue(
        &plan,
        "disk.d0.match",
        "matches 2 disks, but must match exactly one: /dev/sda, /dev/sdb",
    );

    let plan = plan_text(&one_disk(by_id, Some("1TiB")), &sys);
    assert_issue(
        &plan,
        "disk.d0.match",
        "matches only disks smaller than min_size 1 TiB",
    );

    // Regression: a disk sold as 4TB is smaller than 4TiB (ADR 0014).
    let plan = plan_text(&one_disk("/dev/sda", Some("4TB")), &sys);
    assert!(plan.issues.is_empty(), "{:#?}", plan.issues);
    let plan = plan_text(&one_disk("/dev/sda", Some("4TiB")), &sys);
    assert_issue(
        &plan,
        "disk.d0.match",
        "matches only disks smaller than min_size 4 TiB",
    );

    // `min_size` narrows the candidates to exactly one.
    let sys = Fake::example().disk("/dev/sdc", 2 * TIB);
    let plan = plan_text(&one_disk("/dev/sd?", Some("3TiB")), &sys);
    assert_issue(&plan, "disk.d0.match", "matches 2 disks");
    let sys = Fake::default()
        .disk("/dev/sda", 4 * TIB)
        .disk("/dev/sdc", 2 * TIB);
    let plan = plan_text(&one_disk("/dev/sd?", Some("3TiB")), &sys);
    assert!(plan.issues.is_empty(), "{:#?}", plan.issues);
    assert_eq!(
        plan.disks.values().next().unwrap().path,
        Path::new("/dev/sda")
    );
}

#[test]
fn kernel_names_are_warned_about() {
    let plan = plan_text(&one_disk("/dev/sda", None), &Fake::example());
    assert!(plan.issues.is_empty(), "{:#?}", plan.issues);
    assert_eq!(
        plan.warnings,
        ["disk.d0.match.path: `/dev/sda` is a kernel name, which may change between boots"]
    );
}

#[test]
fn two_disks_must_not_resolve_to_one_device() {
    let text = format!(
        "{}  d1:\n    match: {{path: /dev/disk/by-path/pci-0000:00:17.0-ata-1}}\n    table: gpt\n    partitions: []\n",
        one_disk("/dev/sda", None)
    );
    let plan = plan_text(&text, &Fake::example());
    assert_issue(
        &plan,
        "disk.d1.match",
        "resolves to /dev/sda, the same device as disk.d0",
    );
}

#[test]
fn sizes_must_fit_the_disk() {
    let text = one_disk("/dev/sda", None).replace("size: 100%", "size: 8TiB");
    let plan = plan_text(&text, &Fake::example());
    assert_issue(&plan, "disk.d0.partitions", "fixed sizes do not fit");
    assert!(plan.operations.is_empty());
}

#[test]
fn partially_created_groups_are_refused_and_others_are_planned() {
    // ADR 0011: a group where only some disks are empty is refused.
    let sys = Fake::example().table("/dev/sda", &[(1, 3_815_446, LINUX, "data0")]);
    let plan = plan_text(&example_text(), &sys);
    assert_issue(
        &plan,
        "disk.data0",
        "the group of disk.data0 (/dev/sda), disk.data1 (/dev/sdb) is neither empty nor \
         configured as declared, so apply would refuse it (ADR 0011)",
    );
    assert_issue(
        &plan,
        "disk.data1",
        "/dev/sdb has no partition table, but the declaration needs gpt",
    );
    // data1 shares the btrfs filesystem with data0, so it is left alone too.
    let ops = operations(&plan);
    assert!(!ops.iter().any(|o| o.contains("data")), "{ops:#?}");
    assert!(ops.iter().any(|o| o.contains("disk.sys0")));

    // A disk that only holds another device is in use, not empty.
    let state = DiskState {
        holders: vec!["dm-0".to_owned()],
        ..DiskState::default()
    };
    let plan = plan_text(&example_text(), &Fake::example().state("/dev/sdb", state));
    assert_issue(&plan, "disk.data1", "/dev/sdb itself is used by dm-0");
}

#[test]
fn configured_groups_are_left_unchanged() {
    let plan = plan_text(&example_text(), &Fake::configured());
    assert!(plan.issues.is_empty(), "{:#?}", plan.issues);
    assert!(plan.operations.is_empty(), "{:#?}", plan.operations);
    let shown = plan.to_string();
    assert!(shown.contains("already configured: disk.data0, disk.data1\n"));
    assert!(shown.contains("already configured: disk.sys0\n"));

    // An empty group next to a configured one is created.
    let mut sys = Fake::configured();
    sys.states.remove(Path::new("/dev/nvme0n1"));
    sys.volume_groups.clear();
    let plan = plan_text(&example_text(), &sys);
    assert!(plan.issues.is_empty(), "{:#?}", plan.issues);
    let ops = operations(&plan);
    assert!(ops.iter().all(|o| !o.contains("data")), "{ops:#?}");
    assert!(ops.iter().any(|o| o.contains("disk.sys0")));
}

/// Plans the example on `sys`, expecting the group of `disk` to be refused
/// for the issue at `at`.
#[track_caller]
fn assert_refused(sys: &Fake, disk: &str, at: &str, message: &str) -> Plan {
    let plan = plan_text(&example_text(), sys);
    assert_issue(&plan, disk, "neither empty nor configured as declared");
    assert_issue(&plan, at, message);
    assert!(plan.operations.is_empty(), "{:#?}", plan.operations);
    plan
}

#[test]
fn partitions_are_compared() {
    // Sizes may differ from the computed ones by up to 1MiB (ADR 0002).
    let mut sys = Fake::configured();
    sys.partition("/dev/nvme0n1", 3).size += MIB;
    let plan = plan_text(&example_text(), &sys);
    assert!(plan.issues.is_empty(), "{:#?}", plan.issues);
    sys.partition("/dev/nvme0n1", 3).size += 1;
    assert_refused(
        &sys,
        "disk.sys0",
        "disk.sys0.partitions[2]",
        "partition 3 is 929.5 GiB (998056656897 bytes) at 2049 MiB, but the declaration needs \
         951820 MiB at 2049 MiB",
    );

    let mut sys = Fake::configured();
    sys.partition("/dev/nvme0n1", 1).type_guid = LINUX.to_owned();
    sys.partition("/dev/nvme0n1", 2).name = String::new();
    let plan = assert_refused(
        &sys,
        "disk.sys0",
        "disk.sys0.partitions[0]",
        &format!("partition 1 has type {LINUX}, but the declaration needs esp ({ESP})"),
    );
    assert_issue(
        &plan,
        "disk.sys0.partitions[1].label",
        "partition 2 has no label, but the declaration needs label \"boot\"",
    );

    let sys = Fake::configured().table(
        "/dev/sda",
        &[(1, 1024, LINUX, "data0"), (1025, 1024, LINUX, "")],
    );
    let plan = assert_refused(
        &sys,
        "disk.data0",
        "disk.data0.partitions",
        "/dev/sda has partitions [1, 2], but the declaration needs [1]",
    );
    assert_issue(
        &plan,
        "disk.data0.partitions[0]",
        "partition 1 is 1 GiB at 1 MiB",
    );

    let sys = Fake::configured().probe("/dev/sda", probe("ext4", ""));
    assert_refused(
        &sys,
        "disk.data0",
        "disk.data0",
        "/dev/sda itself has a ext4 signature",
    );
}

#[test]
fn luks_is_compared() {
    let mut sys = Fake::configured();
    sys.keys.clear();
    assert_refused(
        &sys,
        "disk.sys0",
        "luks.cryptsys.keyfile",
        "`/run/keys/sys.key` opens no keyslot of disk.sys0.crypt (/dev/nvme0n1p3)",
    );

    let sys = Fake::configured().probe(
        "/dev/nvme0n1p3",
        Probe {
            version: Some("1".to_owned()),
            ..probe("crypto_LUKS", "")
        },
    );
    assert_refused(
        &sys,
        "disk.sys0",
        "luks.cryptsys",
        "has a LUKS1 header, but the declaration needs a LUKS2 header",
    );
}

#[test]
fn what_cannot_be_read_without_changing_state_is_not_configured() {
    // ADR 0011: plan does not open LUKS, so the layers on a closed one are
    // not compared and the group is refused. The other group is unaffected.
    let mut sys = Fake::configured();
    sys.mappings.clear();
    let plan = assert_refused(
        &sys,
        "disk.sys0",
        "luks.cryptsys",
        "disk.sys0.crypt (/dev/nvme0n1p3) is not open, so what is built on it cannot be \
         compared without opening it (ADR 0011)",
    );
    assert_eq!(plan.issues.len(), 2, "{:#?}", plan.issues);
    let data = ["data0", "data1"].map(|d| Name::try_from(d.to_owned()).unwrap());
    assert_eq!(plan.configured, [BTreeSet::from(data)]);

    // ADR 0015: neither does it activate logical volumes.
    let mut sys = Fake::configured();
    sys.vg0().volumes.get_mut("var").unwrap().device = None;
    let plan = assert_refused(
        &sys,
        "disk.sys0",
        "lvm.vg0.volumes.var",
        "the logical volume is not active",
    );
    assert_eq!(plan.issues.len(), 2, "{:#?}", plan.issues);

    // Nor does it mount btrfs.
    let mut sys = Fake::configured();
    sys.btrfs.clear();
    assert_refused(
        &sys,
        "disk.data0",
        "filesystem.data",
        &format!("btrfs {DATA_UUID} is not mounted"),
    );
}

#[test]
fn closed_luks_with_nothing_on_it_is_configured() {
    // Nothing is left uncompared, so the group is configured.
    let text = "version: 1
disk:
  sys0:
    match: {path: /dev/disk/by-id/nvme-Samsung_SSD_980_PRO_1TB_S5GXNX0R123456}
    table: gpt
    partitions:
    - {name: esp, size: 1GiB, type: esp, label: esp}
    - {name: boot, size: 1GiB, label: boot}
    - {name: crypt, size: 100%, label: cryptsys}
luks:
  cryptsys: {device: disk.sys0.crypt, keyfile: /run/keys/sys.key}
filesystem:
  boot: {device: disk.sys0.boot, format: ext4, label: boot}
";
    let mut sys = Fake::configured();
    sys.mappings.clear();
    let plan = plan_text(text, &sys);
    assert!(plan.issues.is_empty(), "{:#?}", plan.issues);
    assert_eq!(plan.configured.len(), 1);
}

#[test]
fn volume_groups_are_compared() {
    let mut sys = Fake::configured();
    let extra = LogicalVolumeState {
        size: GIB,
        device: None,
    };
    sys.vg0().volumes.insert("home".to_owned(), extra);
    sys.vg0().volumes.get_mut("root").unwrap().size += 4 * MIB + 1;
    sys.vg0().extent_size = MIB;
    let plan = assert_refused(
        &sys,
        "disk.sys0",
        "lvm.vg0.volumes",
        "logical volumes that are not declared exist: home",
    );
    assert_issue(
        &plan,
        "lvm.vg0.volumes.root",
        "the logical volume is 64.0 GiB (68723671041 bytes), but the declaration needs 64 GiB",
    );
    assert_issue(
        &plan,
        "lvm.vg0",
        "the physical extent size is 1 MiB, but must be 4 MiB",
    );

    let mut sys = Fake::configured();
    sys.vg0().devices = paths(["/dev/dm-0", "/dev/sdc1"]);
    assert_refused(
        &sys,
        "disk.sys0",
        "lvm.vg0.devices",
        "the physical volumes are /dev/dm-0, /dev/sdc1, but the declaration needs /dev/dm-0",
    );

    let mut sys = Fake::configured();
    sys.volume_groups.clear();
    assert_refused(
        &sys,
        "disk.sys0",
        "lvm.vg0",
        "no volume group named `vg0` exists",
    );
}

#[test]
fn filesystems_and_swap_are_compared() {
    let sys = Fake::configured()
        .probe("/dev/dm-1", probe("ext4", ""))
        .probe("/dev/dm-3", probe("ext4", "var"))
        .probe("/dev/dm-2", probe("swap", "other"));
    let plan = assert_refused(
        &sys,
        "disk.sys0",
        "filesystem.root.label",
        "lvm.vg0.root (/dev/dm-1) has no label, but the declaration needs label \"root\"",
    );
    assert_issue(
        &plan,
        "filesystem.var.format",
        "lvm.vg0.var (/dev/dm-3) has a ext4 signature, but the declaration needs xfs",
    );
    assert_issue(
        &plan,
        "swap.main.label",
        "has label \"other\", but the declaration needs label \"swap\"",
    );

    let mut sys = Fake::configured();
    sys.data().data_profiles = BTreeSet::from(["raid1".to_owned(), "single".to_owned()]);
    sys.data().subvolumes.remove("@srv");
    sys.data().devices.insert("/dev/sdc1".into());
    let plan = assert_refused(
        &sys,
        "disk.data0",
        "filesystem.data.btrfs.data_profile",
        "stores data as raid1 and single, but the declaration needs raid1",
    );
    assert_issue(
        &plan,
        "filesystem.data.subvolumes.srv",
        "has no subvolume `@srv`",
    );
    assert_issue(
        &plan,
        "filesystem.data.devices",
        "/dev/sda1, /dev/sdb1, /dev/sdc1",
    );

    let mut sys = Fake::configured();
    sys.probes.get_mut(Path::new("/dev/sdb1")).unwrap().uuid = Some("other".to_owned());
    assert_refused(
        &sys,
        "disk.data0",
        "filesystem.data.devices",
        "the devices belong to different btrfs filesystems",
    );
}

#[test]
fn keyfiles_must_be_usable() {
    let mut sys = Fake::example();
    sys.keyfiles.clear();
    let plan = plan_text(&example_text(), &sys);
    assert_issue(
        &plan,
        "luks.cryptsys.keyfile",
        "`/run/keys/sys.key` does not exist",
    );
}

#[test]
fn names_must_not_exist_on_the_machine() {
    let mut sys = Fake::example();
    sys.volume_groups = Fake::configured().volume_groups;
    sys.mappers = vec!["cryptsys".to_owned(), "vg0-root".to_owned()];
    let plan = plan_text(&example_text(), &sys);
    assert_issue(
        &plan,
        "lvm.vg0",
        "a volume group named `vg0` already exists",
    );
    assert_issue(
        &plan,
        "luks.cryptsys",
        "a device-mapper device named `cryptsys` already exists",
    );
    assert_issue(
        &plan,
        "lvm.vg0.volumes.root",
        "a device-mapper device named `vg0-root` already exists",
    );
}

#[test]
fn unused_devices_are_listed() {
    let text = one_disk("/dev/sda", None);
    let plan = plan_text(&text, &Fake::example());
    assert_eq!(
        plan.unused
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["disk.d0.p1"]
    );
    assert!(plan.to_string().contains("unused devices:\n  disk.d0.p1\n"));
}

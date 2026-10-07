//! `plan` against a fake machine. The real machine is read only through
//! `System`, so these tests need neither privileges nor devices.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use diskform::layout::MIB;
use diskform::load;
use diskform::model::Name;
use diskform::plan::{Plan, plan};
use diskform::system::{BlockDevice, DeviceKind, DiskState, System};

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
    volume_groups: Vec<String>,
    mappers: Vec<String>,
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

    fn volume_groups(&self) -> Result<Vec<String>, String> {
        Ok(self.volume_groups.clone())
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
fn non_empty_groups_are_refused_and_others_are_planned() {
    let state = DiskState {
        signatures: vec!["gpt partition table".to_owned()],
        partitions: vec!["sda1".to_owned()],
        holders: Vec::new(),
    };
    let sys = Fake::example().state("/dev/sda", state);
    let plan = plan_text(&example_text(), &sys);
    assert_eq!(plan.issues.len(), 1, "{:#?}", plan.issues);
    assert_issue(
        &plan,
        "disk.data0",
        "/dev/sda is not empty (gpt partition table, partition sda1); judging whether existing storage satisfies the declaration is not implemented yet (ADR 0011)",
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
    assert_issue(&plan, "disk.data1", "(used by dm-0)");
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
    sys.volume_groups = vec!["vg0".to_owned()];
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

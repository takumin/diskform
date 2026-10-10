//! `destroy` against a fake machine: what it reads, refuses and would do,
//! and how it performs the steps through a fake `Change`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use diskform::change::Change;
use diskform::destroy::{Destruction, Targets, destroy, execute};
use diskform::load;
use diskform::system::{
    BlockDevice, BtrfsMembers, BtrfsState, DiskState, Node, NodeKind, PartitionTable, Probe,
    System, VolumeGroupState,
};

use crate::fake::*;
use crate::{examples, run, write};

fn destroy_on(sys: &dyn System, targets: &[&str]) -> Destruction {
    let decl = load::load(&examples()[0]).unwrap();
    let targets = if targets == ["--all"] {
        Targets::All
    } else {
        Targets::Disks(targets.iter().map(|t| (*t).to_owned()).collect())
    };
    destroy(&decl, sys, &targets)
}

fn steps(d: &Destruction) -> Vec<String> {
    d.steps.iter().map(ToString::to_string).collect()
}

#[track_caller]
fn assert_refused(d: &Destruction, at: &str, message: &str) {
    assert!(d.steps.is_empty(), "{:#?}", d.steps);
    assert!(!d.to_string().contains("nothing to destroy"));
    assert!(
        d.issues
            .iter()
            .any(|i| i.at == at && i.message.contains(message)),
        "missing {at}: {message:?} in {:#?}",
        d.issues
    );
}

/// The configured machine with nothing mounted, so that destroy can
/// erase it.
fn unmounted() -> Fake {
    let mut sys = Fake::configured();
    sys.btrfs.clear();
    sys
}

const ALL_STEPS: [&str; 10] = [
    "deactivate volume group vg0",
    "close LUKS mapping cryptsys (/dev/dm-0) on /dev/nvme0n1p3",
    "wipe btrfs signature on /dev/sda1",
    "wipe btrfs signature on /dev/sdb1",
    "wipe vfat signature on /dev/nvme0n1p1",
    "wipe ext4 signature on /dev/nvme0n1p2",
    "wipe crypto_LUKS (version 2) signature on /dev/nvme0n1p3",
    "wipe gpt partition table on disk.data0 (/dev/sda)",
    "wipe gpt partition table on disk.data1 (/dev/sdb)",
    "wipe gpt partition table on disk.sys0 (/dev/nvme0n1)",
];

#[test]
fn configured_disks_are_torn_down_from_the_top_and_wiped() {
    let d = destroy_on(&unmounted(), &["--all"]);
    assert!(d.issues.is_empty(), "{:#?}", d.issues);
    assert_eq!(steps(&d), ALL_STEPS);
    assert!(d.missing.is_empty());
    assert_eq!(d.disks.len(), 3);
}

#[test]
fn only_the_targets_are_erased() {
    let d = destroy_on(&unmounted(), &["disk.sys0"]);
    assert!(d.issues.is_empty(), "{:#?}", d.issues);
    assert_eq!(
        steps(&d),
        [
            "deactivate volume group vg0",
            "close LUKS mapping cryptsys (/dev/dm-0) on /dev/nvme0n1p3",
            "wipe vfat signature on /dev/nvme0n1p1",
            "wipe ext4 signature on /dev/nvme0n1p2",
            "wipe crypto_LUKS (version 2) signature on /dev/nvme0n1p3",
            "wipe gpt partition table on disk.sys0 (/dev/nvme0n1)",
        ]
    );
    let d = destroy_on(&unmounted(), &["disk.data0", "disk.data1"]);
    assert!(d.issues.is_empty(), "{:#?}", d.issues);
    assert_eq!(steps(&d).len(), 4);
}

#[test]
fn empty_disks_need_nothing() {
    let d = destroy_on(&Fake::example(), &["--all"]);
    assert!(d.issues.is_empty(), "{:#?}", d.issues);
    assert!(d.steps.is_empty());
    assert!(d.to_string().contains("nothing to destroy"));
}

#[test]
fn targets_must_name_disks_of_the_declaration() {
    for target in ["sys0", "disk.nvme", "luks.cryptsys"] {
        let d = destroy_on(&unmounted(), &["disk.data0", target]);
        assert_refused(&d, "--target", &format!("`{target}` is not a disk"));
    }
}

#[test]
fn every_disk_of_the_declaration_must_be_resolved() {
    let mut sys = unmounted();
    sys.links
        .remove(Path::new("/dev/disk/by-path/pci-0000:00:17.0-ata-2"));
    let d = destroy_on(&sys, &["disk.sys0"]);
    assert_refused(&d, "disk.data1.match", "no device matches");
}

#[test]
fn storage_spanning_targets_and_other_declared_disks_is_refused() {
    let d = destroy_on(&unmounted(), &["disk.data0"]);
    assert_refused(
        &d,
        "disk.data0",
        &format!(
            "the btrfs {DATA_UUID} on it is also on /dev/sdb1 of disk.data1 (/dev/sdb); \
             add `--target disk.data1` or use `--all`"
        ),
    );
}

#[test]
fn storage_spanning_undeclared_disks_is_refused() {
    // vg0 was extended to a disk that the declaration does not have.
    let mut sys = unmounted()
        .disk("/dev/sdc", TIB)
        .table("/dev/sdc", &[(1, 1024, LINUX, "")])
        .probe("/dev/sdc1", probe("LVM2_member", ""));
    sys.vg0().devices.insert("/dev/sdc1".into());
    let d = destroy_on(&sys, &["--all"]);
    assert_refused(
        &d,
        "disk.sys0",
        "the volume group `vg0` on it is also on /dev/sdc1 of /dev/sdc, which is not in the \
         declaration",
    );

    // Only the btrfs signature on a disk outside the declaration is found.
    let mut sys = unmounted()
        .disk("/dev/sdc", TIB)
        .table("/dev/sdc", &[(1, 1024, LINUX, "")]);
    let member = Probe {
        uuid: Some(DATA_UUID.to_owned()),
        ..probe("btrfs", "data")
    };
    sys = sys.probe("/dev/sdc1", member);
    let d = destroy_on(&sys, &["--all"]);
    assert_refused(
        &d,
        "disk.data0",
        "is also on /dev/sdc1 of /dev/sdc, which is not in the declaration",
    );
}

#[test]
fn missing_members_are_shown() {
    let mut sys = unmounted();
    sys.vg0().devices.insert("[unknown]".into());
    sys.btrfs_counts.insert(DATA_UUID.to_owned(), 3);
    let d = destroy_on(&sys, &["--all"]);
    assert!(d.issues.is_empty(), "{:#?}", d.issues);
    assert_eq!(
        d.missing,
        [
            "the volume group `vg0` lacks some of its physical volumes".to_owned(),
            format!("the btrfs {DATA_UUID} lacks 1 of its 3 devices"),
        ]
    );
    assert!(d.to_string().contains("missing members:\n"));
}

#[test]
fn what_is_in_use_is_refused() {
    for (device, usage, at) in [
        ("/dev/dm-1", "mounted at /", "disk.sys0"),
        ("/dev/dm-2", "active swap", "disk.sys0"),
        ("/dev/nvme0n1p1", "mounted at /boot/efi", "disk.sys0"),
        (
            "/dev/sdb1",
            "a device of the mounted btrfs 5d1c7e2a",
            "disk.data1",
        ),
    ] {
        let mut sys = unmounted();
        sys.uses.insert(device.into(), usage.to_owned());
        let d = destroy_on(&sys, &["--all"]);
        assert_refused(
            &d,
            at,
            &format!("{device} is {usage}; destroy does not stop what is in use"),
        );
    }
}

#[test]
fn unsupported_stacks_are_refused() {
    let sys = unmounted().stack("/dev/md127", NodeKind::Md, &["/dev/sda1", "/dev/sdb1"]);
    let d = destroy_on(&sys, &["--all"]);
    assert_refused(&d, "disk.data0", "/dev/md127 is an md array");

    let sys = unmounted().probe("/dev/nvme0n1p2", probe("linux_raid_member", ""));
    let d = destroy_on(&sys, &["--all"]);
    assert_refused(&d, "disk.sys0", "/dev/nvme0n1p2 is a member of an md array");

    let kind = NodeKind::OtherMapper {
        name: "mpatha".to_owned(),
        uuid: "mpath-3600".to_owned(),
    };
    let sys = unmounted().stack("/dev/dm-9", kind, &["/dev/sda"]);
    let d = destroy_on(&sys, &["--all"]);
    assert_refused(
        &d,
        "disk.data0",
        "/dev/dm-9 is the device-mapper device `mpatha` of an unknown kind",
    );
}

#[test]
fn partitions_unknown_to_the_kernel_are_refused() {
    let mut sys = unmounted();
    sys.states
        .get_mut(Path::new("/dev/sda"))
        .unwrap()
        .partitions
        .clear();
    let d = destroy_on(&sys, &["--all"]);
    assert_refused(
        &d,
        "disk.data0",
        "the partitions that the kernel knows on /dev/sda differ from the partition table",
    );
}

#[test]
fn a_disk_without_a_table_is_wiped_whole() {
    // LUKS directly on the disk, which an earlier machine may have used.
    let sys = unmounted().state(
        "/dev/sda",
        DiskState {
            signatures: vec!["crypto_LUKS signature".to_owned()],
            ..DiskState::default()
        },
    );
    let mut sys = sys.probe("/dev/sda", probe("crypto_LUKS", ""));
    sys.tables.remove(Path::new("/dev/sda"));
    sys.probes.remove(Path::new("/dev/sda1"));
    let d = destroy_on(&sys, &["disk.data0"]);
    assert!(d.issues.is_empty(), "{:#?}", d.issues);
    assert_eq!(
        steps(&d),
        ["wipe crypto_LUKS signature on disk.data0 (/dev/sda)"]
    );
}

#[test]
fn a_changed_machine_is_not_the_confirmed_one() {
    let shown = destroy_on(&unmounted(), &["--all"]);
    assert!(shown.same_as(&destroy_on(&unmounted(), &["--all"])));

    let sys = unmounted().probe("/dev/sdb1", probe("ext4", ""));
    assert!(!shown.same_as(&destroy_on(&sys, &["--all"])));

    let mut sys = unmounted();
    sys.uses
        .insert("/dev/dm-1".into(), "mounted at /".to_owned());
    assert!(!shown.same_as(&destroy_on(&sys, &["--all"])));
}

/// A fake machine that `Change` changes, recording each change.
struct Machine {
    fake: RefCell<Fake>,
    changes: RefCell<Vec<String>>,
    /// A device whose wipe fails.
    broken: Option<PathBuf>,
}

impl Machine {
    fn new(fake: Fake) -> Self {
        Machine {
            fake: RefCell::new(fake),
            changes: RefCell::new(Vec::new()),
            broken: None,
        }
    }
}

impl System for Machine {
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
        self.fake.borrow().read_dir(dir)
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        self.fake.borrow().canonicalize(path)
    }
    fn block_device(&self, path: &Path) -> io::Result<Option<BlockDevice>> {
        self.fake.borrow().block_device(path)
    }
    fn disk_state(&self, disk: &BlockDevice) -> Result<DiskState, String> {
        self.fake.borrow().disk_state(disk)
    }
    fn partition_table(&self, disk: &BlockDevice) -> Result<Option<PartitionTable>, String> {
        self.fake.borrow().partition_table(disk)
    }
    fn probe(&self, device: &Path) -> Result<Probe, String> {
        System::probe(&*self.fake.borrow(), device)
    }
    fn check_keyfile(&self, path: &Path) -> Result<(), String> {
        self.fake.borrow().check_keyfile(path)
    }
    fn test_keyfile(&self, device: &Path, keyfile: &Path) -> Result<bool, String> {
        self.fake.borrow().test_keyfile(device, keyfile)
    }
    fn luks_mapping(&self, device: &Path) -> Result<Option<PathBuf>, String> {
        self.fake.borrow().luks_mapping(device)
    }
    fn volume_groups(&self) -> Result<BTreeMap<String, VolumeGroupState>, String> {
        self.fake.borrow().volume_groups()
    }
    fn btrfs(&self, uuid: &str) -> Result<Option<BtrfsState>, String> {
        self.fake.borrow().btrfs(uuid)
    }
    fn mapper_names(&self) -> Result<Vec<String>, String> {
        self.fake.borrow().mapper_names()
    }
    fn node(&self, device: &Path) -> Result<Node, String> {
        self.fake.borrow().node(device)
    }
    fn uses(&self) -> Result<BTreeMap<PathBuf, String>, String> {
        self.fake.borrow().uses()
    }
    fn btrfs_members(&self, uuid: &str) -> Result<BtrfsMembers, String> {
        self.fake.borrow().btrfs_members(uuid)
    }
}

impl Change for Machine {
    fn deactivate_volume_group(&self, name: &str) -> Result<(), String> {
        self.changes.borrow_mut().push(format!("vgchange {name}"));
        let mut fake = self.fake.borrow_mut();
        let devices: Vec<PathBuf> = fake
            .volume_groups
            .get_mut(name)
            .ok_or("no such volume group")?
            .volumes
            .values_mut()
            .filter_map(|v| v.device.take())
            .collect();
        for device in devices {
            fake.stacked.remove(&device);
        }
        Ok(())
    }

    fn close_luks(&self, name: &str) -> Result<(), String> {
        self.changes.borrow_mut().push(format!("close {name}"));
        let mut fake = self.fake.borrow_mut();
        let mapping = fake
            .stacked
            .iter()
            .find(|(_, (k, _))| *k == NodeKind::Luks { name: name.into() })
            .map(|(p, _)| p.clone())
            .ok_or("no such mapping")?;
        if fake
            .stacked
            .values()
            .any(|(_, lower)| lower.contains(&mapping))
        {
            return Err(format!("{} is in use", mapping.display()));
        }
        fake.stacked.remove(&mapping);
        fake.mappings.retain(|_, m| *m != mapping);
        Ok(())
    }

    fn wipe(&self, device: &Path) -> Result<(), String> {
        self.changes
            .borrow_mut()
            .push(format!("wipefs {}", device.display()));
        if self.broken.as_deref() == Some(device) {
            return Err(format!("{}: Device or resource busy", device.display()));
        }
        let mut fake = self.fake.borrow_mut();
        if fake
            .stacked
            .values()
            .any(|(_, lower)| lower.iter().any(|l| l == device))
        {
            return Err(format!("{}: Device or resource busy", device.display()));
        }
        fake.probes.remove(device);
        if fake.tables.remove(device).is_some() || fake.states.contains_key(device) {
            // As on a loop device without partition scanning, the kernel
            // keeps the partitions until they are deleted.
            if let Some(state) = fake.states.get_mut(device) {
                state.signatures.clear();
            }
        }
        Ok(())
    }

    fn forget_partitions(&self, disk: &Path) -> Result<(), String> {
        self.changes
            .borrow_mut()
            .push(format!("partx --delete {}", disk.display()));
        if let Some(state) = self.fake.borrow_mut().states.get_mut(disk) {
            state.partitions.clear();
        }
        Ok(())
    }
}

#[test]
fn steps_are_performed_in_order_and_leave_the_disks_empty() {
    let machine = Machine::new(unmounted());
    let d = destroy_on(&machine, &["--all"]);
    let mut done = Vec::new();
    execute(&d, &machine, &machine, &mut |s| done.push(s.to_string())).unwrap();
    assert_eq!(done, ALL_STEPS);
    assert_eq!(
        *machine.changes.borrow(),
        [
            "vgchange vg0",
            "close cryptsys",
            "wipefs /dev/sda1",
            "wipefs /dev/sdb1",
            "wipefs /dev/nvme0n1p1",
            "wipefs /dev/nvme0n1p2",
            "wipefs /dev/nvme0n1p3",
            "wipefs /dev/sda",
            "partx --delete /dev/sda",
            "wipefs /dev/sdb",
            "partx --delete /dev/sdb",
            "wipefs /dev/nvme0n1",
            "partx --delete /dev/nvme0n1",
        ]
    );
    for disk in d.disks.values() {
        assert!(machine.disk_state(disk).unwrap().is_empty());
    }
    // Destroying again finds nothing to do.
    assert!(destroy_on(&machine, &["--all"]).steps.is_empty());
}

#[test]
fn a_failure_stops_at_the_failed_step() {
    let mut machine = Machine::new(unmounted());
    machine.broken = Some("/dev/nvme0n1p2".into());
    let d = destroy_on(&machine, &["--all"]);
    let mut done = Vec::new();
    let e = execute(&d, &machine, &machine, &mut |s| done.push(s.to_string())).unwrap_err();
    assert_eq!(done, ALL_STEPS[..5]);
    assert_eq!(
        e,
        "wipe ext4 signature on /dev/nvme0n1p2: /dev/nvme0n1p2: Device or resource busy"
    );
    assert_eq!(machine.changes.borrow().len(), 6);

    // Destroying again starts from the state that the failure left.
    machine.broken = None;
    let d = destroy_on(&machine, &["--all"]);
    assert_eq!(steps(&d), ALL_STEPS[5..]);
}

#[test]
fn a_step_that_does_not_take_effect_is_a_failure() {
    struct Ignoring(Machine);
    impl Change for Ignoring {
        fn deactivate_volume_group(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn close_luks(&self, name: &str) -> Result<(), String> {
            self.0.close_luks(name)
        }
        fn wipe(&self, device: &Path) -> Result<(), String> {
            self.0.wipe(device)
        }
        fn forget_partitions(&self, disk: &Path) -> Result<(), String> {
            self.0.forget_partitions(disk)
        }
    }
    let changer = Ignoring(Machine::new(unmounted()));
    let d = destroy_on(&changer.0, &["--all"]);
    let e = execute(&d, &changer.0, &changer, &mut |_| {}).unwrap_err();
    assert_eq!(
        e,
        "deactivate volume group vg0: some of its logical volumes are still active"
    );
}

#[test]
fn targets_are_required_and_exclusive() {
    let file = write("yaml", "version: 1\n");
    let out = run(&["destroy"], &file);
    assert!(!out.success);
    assert!(out.stderr.contains("--target"), "{}", out.stderr);
    let out = run(&["destroy", "--all", "--target", "disk.a"], &file);
    assert!(!out.success);
    assert!(out.stderr.contains("cannot be used with"), "{}", out.stderr);
}

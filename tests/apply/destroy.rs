//! `destroy` against storage built like the example (ADR 0004, ADR 0016).

use std::fs;
use std::path::Path;

use crate::guest::{disk, diskform, partition, run, signatures};
use crate::{GIB, MIB, Test};

const DISKS: &[(&str, u64)] = &[
    ("sys0", 2 * GIB),
    ("data0", 512 * MIB),
    ("data1", 512 * MIB),
];
const DECLARATION: &str = "/run/diskform.yaml";
const KEYFILE: &str = "/run/keys/sys.key";

/// The example `luks-lvm-btrfs-raid1`, with the disks of the VM and sizes
/// that fit them.
const EXAMPLE: &str = "\
version: 1
disk:
  sys0:
    match: {path: /dev/disk/by-id/virtio-sys0}
    table: gpt
    partitions:
    - {name: esp, size: 64MiB, type: esp, label: esp}
    - {name: boot, size: 256MiB, label: boot}
    - {name: crypt, size: 100%, label: cryptsys}
  data0:
    match: {path: /dev/disk/by-id/virtio-data0}
    table: gpt
    partitions:
    - {name: data0, size: 100%, label: data0}
  data1:
    match: {path: /dev/disk/by-id/virtio-data1}
    table: gpt
    partitions:
    - {name: data1, size: 100%, label: data1}
luks:
  cryptsys: {device: disk.sys0.crypt, keyfile: /run/keys/sys.key}
lvm:
  vg0:
    devices: [luks.cryptsys]
    volumes:
      root: {size: 512MiB}
      swap: {size: 64MiB}
      var: {size: 100%}
filesystem:
  efi: {device: disk.sys0.esp, format: vfat, label: EFI, mount: /boot/efi}
  boot: {device: disk.sys0.boot, format: ext4, label: boot, mount: /boot}
  root: {device: lvm.vg0.root, format: ext4, label: root, mount: /}
  var: {device: lvm.vg0.var, format: xfs, label: var, mount: /var}
  data:
    devices: [disk.data0.data0, disk.data1.data1]
    format: btrfs
    label: data
    btrfs: {data_profile: raid1, metadata_profile: raid1}
swap:
  main: {device: lvm.vg0.swap, label: swap}
";

/// Builds the example with the usual tools, since apply does not exist
/// yet, and leaves the LUKS mapping open and the volume group active.
fn build_example() {
    fs::create_dir_all(Path::new(KEYFILE).parent().unwrap()).unwrap();
    fs::write(KEYFILE, "diskform apply-layer test key").unwrap();
    fs::write(DECLARATION, EXAMPLE).unwrap();

    run(
        "sgdisk",
        &[
            "--new=1:0:+64M",
            "--typecode=1:ef00",
            "--change-name=1:esp",
            "--new=2:0:+256M",
            "--change-name=2:boot",
            "--new=3:0:0",
            "--change-name=3:cryptsys",
            &disk("sys0"),
        ],
    );
    for name in ["data0", "data1"] {
        run(
            "sgdisk",
            &[
                "--new=1:0:0",
                &format!("--change-name=1:{name}"),
                &disk(name),
            ],
        );
    }
    run("udevadm", &["settle"]);

    run("mkfs.vfat", &["-n", "EFI", &partition("sys0", 1)]);
    run("mkfs.ext4", &["-q", "-L", "boot", &partition("sys0", 2)]);
    // A cheap key derivation keeps the test fast; the strength of the key
    // does not matter here.
    run(
        "cryptsetup",
        &[
            "luksFormat",
            "--batch-mode",
            "--pbkdf=pbkdf2",
            "--pbkdf-force-iterations=1000",
            "--key-file",
            KEYFILE,
            &partition("sys0", 3),
        ],
    );
    run(
        "cryptsetup",
        &[
            "open",
            "--key-file",
            KEYFILE,
            &partition("sys0", 3),
            "cryptsys",
        ],
    );
    run("pvcreate", &["/dev/mapper/cryptsys"]);
    run("vgcreate", &["vg0", "/dev/mapper/cryptsys"]);
    run("lvcreate", &["--yes", "-n", "root", "-L", "512M", "vg0"]);
    run("lvcreate", &["--yes", "-n", "swap", "-L", "64M", "vg0"]);
    run("lvcreate", &["--yes", "-n", "var", "-l", "100%FREE", "vg0"]);
    run("mkfs.ext4", &["-q", "-L", "root", "/dev/vg0/root"]);
    run("mkswap", &["-L", "swap", "/dev/vg0/swap"]);
    run("mkfs.xfs", &["-q", "-L", "var", "/dev/vg0/var"]);
    run(
        "mkfs.btrfs",
        &[
            "--quiet",
            "--label",
            "data",
            "--data",
            "raid1",
            "--metadata",
            "raid1",
            &partition("data0", 1),
            &partition("data1", 1),
        ],
    );
    run("udevadm", &["settle"]);
}

/// Checks that the storage that `build_example` built is all still there.
fn assert_example_intact() {
    let expected = [
        (disk("sys0"), "gpt"),
        (partition("sys0", 1), "vfat"),
        (partition("sys0", 2), "ext4"),
        (partition("sys0", 3), "crypto_LUKS"),
        ("/dev/mapper/cryptsys".to_owned(), "LVM2_member"),
        ("/dev/vg0/root".to_owned(), "ext4"),
        ("/dev/vg0/swap".to_owned(), "swap"),
        ("/dev/vg0/var".to_owned(), "xfs"),
        (disk("data0"), "gpt"),
        (partition("data0", 1), "btrfs"),
        (disk("data1"), "gpt"),
        (partition("data1", 1), "btrfs"),
    ];
    for (device, kind) in expected {
        assert!(
            signatures(&device).iter().any(|s| s == kind),
            "{device} lost its {kind} signature"
        );
    }
}

fn destroy_all() -> std::process::Output {
    diskform(&["destroy", "--all", "--auto-approve", DECLARATION])
}

fn assert_refused(out: &std::process::Output, message: &str) {
    assert!(!out.status.success(), "destroy did not refuse");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(message),
        "destroy refused without saying `{message}`"
    );
    assert_example_intact();
}

pub const TEARS_DOWN_THE_EXAMPLE: Test = Test {
    name: "destroy_tears_down_the_example",
    disks: DISKS,
    run: || {
        build_example();
        let out = destroy_all();
        assert!(out.status.success(), "destroy failed");

        for (serial, _) in DISKS {
            let device = disk(serial);
            assert_eq!(signatures(&device), Vec::<String>::new(), "{device}");
            // Only the disk itself, without partitions or devices on it.
            let tree = run(
                "lsblk",
                &["--noheadings", "--raw", "--output", "NAME", &device],
            );
            assert_eq!(tree.lines().count(), 1, "{device} still has\n{tree}");
        }
        let mappers = fs::read_dir("/sys/class/block")
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("dm-")
            })
            .count();
        assert_eq!(mappers, 0, "device-mapper devices are left");

        let again = destroy_all();
        assert!(again.status.success());
        assert!(String::from_utf8_lossy(&again.stdout).contains("nothing to destroy"));
    },
};

pub const REFUSES_A_MOUNTED_BTRFS: Test = Test {
    name: "destroy_refuses_a_mounted_btrfs",
    disks: DISKS,
    run: || {
        build_example();
        fs::create_dir_all("/mnt/data").unwrap();
        run("mount", &[&partition("data0", 1), "/mnt/data"]);
        // Both devices of the btrfs are in use, though only one of them is
        // the source of the mount.
        let out = destroy_all();
        assert_refused(&out, "is a device of the mounted btrfs");
        assert_refused(&out, "is mounted at /mnt/data");
    },
};

pub const REFUSES_AN_ACTIVE_SWAP: Test = Test {
    name: "destroy_refuses_an_active_swap",
    disks: DISKS,
    run: || {
        build_example();
        run("swapon", &["/dev/vg0/swap"]);
        assert_refused(&destroy_all(), "is active swap");
    },
};

pub const REFUSES_A_VOLUME_GROUP_ON_AN_UNDECLARED_DISK: Test = Test {
    name: "destroy_refuses_a_volume_group_on_an_undeclared_disk",
    disks: &[
        ("sys0", 2 * GIB),
        ("data0", 512 * MIB),
        ("data1", 512 * MIB),
        ("extra", 256 * MIB),
    ],
    run: || {
        build_example();
        run("pvcreate", &[&disk("extra")]);
        run("vgextend", &["vg0", &disk("extra")]);
        assert_refused(&destroy_all(), "which is not in the declaration");
    },
};

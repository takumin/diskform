//! The example `luks-lvm-btrfs-raid1` on the disks of the VM.

use std::fs;
use std::path::Path;

use crate::guest::{disk, partition, run, signatures};
use crate::{GIB, MIB};

pub const DISKS: &[(&str, u64)] = &[
    ("sys0", 2 * GIB),
    ("data0", 512 * MIB),
    ("data1", 512 * MIB),
];
pub const DECLARATION: &str = "/run/diskform.yaml";
pub const KEYFILE: &str = "/run/keys/sys.key";

/// The example `luks-lvm-btrfs-raid1`, with the disks of the VM and sizes
/// that fit them.
pub const EXAMPLE: &str = "\
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
    subvolumes:
      srv: {path: '@srv', mount: /srv}
      snapshots: {path: '@snapshots', mount: /srv/.snapshots}
swap:
  main: {device: lvm.vg0.swap, label: swap}
";

/// Writes the declaration of the example and its key file.
pub fn write_example() {
    fs::create_dir_all(Path::new(KEYFILE).parent().unwrap()).unwrap();
    fs::write(KEYFILE, "diskform apply-layer test key").unwrap();
    fs::write(DECLARATION, EXAMPLE).unwrap();
}

/// Builds the example with the usual tools rather than apply, as on disks
/// from another machine, and leaves the LUKS mapping open and the volume
/// group active.
pub fn build_example() {
    write_example();

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
pub fn assert_example_intact() {
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

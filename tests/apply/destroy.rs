//! `destroy` against storage built like the example (ADR 0004, ADR 0016).

use std::fs;

use crate::example::{DECLARATION, DISKS, assert_example_intact, build_example};
use crate::guest::{disk, diskform, partition, run, signatures};
use crate::{GIB, MIB, Test};

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
        // Without this, powering off the VM waits long for the swap.
        run("swapoff", &["/dev/vg0/swap"]);
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

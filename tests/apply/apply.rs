//! `apply` on the disks of the example (ADR 0004, ADR 0011, ADR 0018).

use std::fs;
use std::process::Output;

use crate::Test;
use crate::example::{DECLARATION, DISKS, assert_example_intact, build_example, write_example};
use crate::guest::{disk, diskform, partition, run, signatures};

fn apply() -> Output {
    diskform(&["apply", "--auto-approve", DECLARATION])
}

fn assert_failed(out: &Output, message: &str) {
    assert!(!out.status.success(), "apply did not fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(message),
        "apply failed without saying `{message}`"
    );
}

fn assert_blank(serials: &[&str]) {
    for serial in serials {
        let device = disk(serial);
        assert_eq!(signatures(&device), Vec::<String>::new(), "{device}");
    }
}

/// What is on the disks, to see that nothing changed.
fn snapshot() -> String {
    let columns = "NAME,TYPE,SIZE,FSTYPE,LABEL,UUID,PARTLABEL,PARTTYPE";
    let mut args = vec!["--raw", "--output", columns];
    let disks: Vec<String> = DISKS.iter().map(|(serial, _)| disk(serial)).collect();
    args.extend(disks.iter().map(String::as_str));
    run("lsblk", &args)
}

pub const CREATES_AND_KEEPS_THE_EXAMPLE: Test = Test {
    name: "apply_creates_and_keeps_the_example",
    disks: DISKS,
    run: || {
        write_example();
        // Without a terminal, apply cannot ask, and changes nothing.
        assert_failed(
            &diskform(&["apply", DECLARATION]),
            "standard input is not a terminal",
        );
        assert_blank(&["sys0", "data0", "data1"]);

        let out = apply();
        assert!(out.status.success(), "apply failed");
        assert_example_intact();
        let made = snapshot();

        // ADR 0015: the btrfs is not mounted, so the group of the data
        // disks cannot be judged configured.
        assert_failed(&apply(), "is not mounted");
        assert_eq!(snapshot(), made);

        fs::create_dir_all("/mnt/data").unwrap();
        run("mount", &[&partition("data0", 1), "/mnt/data"]);
        let out = apply();
        assert!(out.status.success(), "apply failed on the configured disks");
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in [
            "already configured: disk.data0, disk.data1",
            "already configured: disk.sys0",
            "nothing to apply",
        ] {
            assert!(stdout.contains(line), "apply did not say `{line}`");
        }
        assert_eq!(snapshot(), made);
        run("umount", &["/mnt/data"]);
    },
};

pub const REFUSES_A_DISK_WITH_A_SIGNATURE: Test = Test {
    name: "apply_refuses_a_disk_with_a_signature",
    disks: DISKS,
    run: || {
        write_example();
        run("mkfs.ext4", &["-q", &disk("data1")]);
        let out = apply();
        assert_failed(&out, "is neither empty nor configured as declared");
        assert_failed(&out, "apply refused; nothing was changed");
        assert_blank(&["sys0", "data0"]);
        assert_eq!(signatures(&disk("data1")), ["ext4"]);
    },
};

pub const STOPS_AT_WHAT_EARLIER_STORAGE_LEFT: Test = Test {
    name: "apply_stops_at_what_earlier_storage_left",
    disks: DISKS,
    run: || {
        // Erasing only the partition tables leaves the signatures in the
        // partitions, where apply makes its own partitions again.
        build_example();
        run("vgchange", &["--activate", "n", "vg0"]);
        run("cryptsetup", &["close", "cryptsys"]);
        for (serial, _) in DISKS {
            run("wipefs", &["--all", &disk(serial)]);
        }
        run("udevadm", &["settle"]);
        assert_blank(&["sys0", "data0", "data1"]);

        let out = apply();
        assert_failed(
            &out,
            "crypto_LUKS (version 2) signature, which earlier storage left",
        );
        assert_failed(&out, "apply stopped");
        // Nothing was written over.
        for (device, kind) in [
            (partition("sys0", 1), "vfat"),
            (partition("sys0", 2), "ext4"),
            (partition("sys0", 3), "crypto_LUKS"),
            (partition("data0", 1), "btrfs"),
            (partition("data1", 1), "btrfs"),
        ] {
            assert_eq!(signatures(&device), [kind], "{device}");
        }

        // ADR 0011: apply does not continue what it left partly done.
        assert_failed(&apply(), "is neither empty nor configured as declared");
        let out = diskform(&["destroy", "--all", "--auto-approve", DECLARATION]);
        assert!(out.status.success(), "destroy failed");
        let out = apply();
        assert!(out.status.success(), "apply failed after destroy");
        assert_example_intact();
    },
};

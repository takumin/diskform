//! `mount` and `unmount` of the example that apply made (ADR 0004,
//! ADR 0019).

use std::fs;
use std::process::Output;

use crate::Test;
use crate::example::{DECLARATION, DISKS, write_example};
use crate::guest::{diskform, mappers, partition, run};

const ROOT: &str = "/mnt/target";

fn mount() -> Output {
    diskform(&["mount", "--root", ROOT, DECLARATION])
}

fn unmount() -> Output {
    diskform(&["unmount", "--root", ROOT, DECLARATION])
}

fn assert_ok(out: &Output, what: &str) {
    assert!(out.status.success(), "{what} failed");
}

fn assert_failed(out: &Output, message: &str) {
    assert!(!out.status.success(), "it did not fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(message),
        "it failed without saying `{message}`"
    );
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The mounts under the root, as `target fstype options` lines.
fn mounted() -> Vec<String> {
    let out = run(
        "findmnt",
        &[
            "--list",
            "--noheadings",
            "--output",
            "TARGET,FSTYPE,OPTIONS",
        ],
    );
    out.lines()
        .filter(|l| l.starts_with(ROOT))
        .map(str::to_owned)
        .collect()
}

/// Makes the example with apply, and stops using it as unmount does.
fn applied_and_closed() {
    write_example();
    fs::create_dir_all(ROOT).unwrap();
    assert_ok(
        &diskform(&["apply", "--auto-approve", DECLARATION]),
        "apply",
    );
    // apply leaves the LUKS mapping open and the volume group active.
    assert!(mappers() > 0);
    let out = unmount();
    assert_ok(&out, "unmount");
    assert_eq!(mappers(), 0, "unmount left device-mapper devices");
}

pub const MOUNTS_AND_UNMOUNTS_THE_EXAMPLE: Test = Test {
    name: "mount_mounts_and_unmounts_the_example",
    disks: DISKS,
    run: || {
        applied_and_closed();

        let out = mount();
        assert_ok(&out, "mount");
        assert!(stdout(&out).contains("done: open luks.cryptsys"));
        let mounts = mounted();
        for (target, fstype, option) in [
            ("/mnt/target ", "ext4", "rw"),
            ("/mnt/target/boot ", "ext4", "rw"),
            // The kernel shows umask=0077 as both masks.
            ("/mnt/target/boot/efi ", "vfat", "fmask=0077,dmask=0077"),
            ("/mnt/target/var ", "xfs", "rw"),
            ("/mnt/target/srv ", "btrfs", "subvol=/@srv"),
            ("/mnt/target/srv/.snapshots ", "btrfs", "subvol=/@snapshots"),
        ] {
            assert!(
                mounts
                    .iter()
                    .any(|m| m.starts_with(target) && m.contains(fstype) && m.contains(option)),
                "{target}is not mounted as {fstype} with {option}: {mounts:#?}"
            );
        }
        assert!(mounts.iter().any(|m| m.contains("compress=zstd:3")));
        assert_eq!(mounts.len(), 6, "{mounts:#?}");

        // ADR 0011: with everything open and mounted, the groups are
        // judged configured, so apply has nothing to do.
        let out = diskform(&["apply", "--auto-approve", DECLARATION]);
        assert_ok(&out, "apply");
        assert!(stdout(&out).contains("nothing to apply"));

        let out = mount();
        assert_ok(&out, "mount again");
        assert!(!stdout(&out).contains("done:"), "mount did something again");
        assert_eq!(mounted().len(), 6);

        assert_ok(&unmount(), "unmount");
        assert_eq!(mounted(), Vec::<String>::new());
        assert_eq!(mappers(), 0, "unmount left device-mapper devices");
        let out = unmount();
        assert_ok(&out, "unmount again");
        assert!(stdout(&out).contains("nothing to unmount"));
    },
};

pub const REFUSES_STORAGE_UNLIKE_THE_DECLARATION: Test = Test {
    name: "mount_refuses_storage_unlike_the_declaration",
    disks: DISKS,
    run: || {
        applied_and_closed();
        run("e2label", &[&partition("sys0", 2), "other"]);
        assert_failed(
            &mount(),
            "has label \"other\", but the declaration needs label \"boot\"",
        );
        // Nothing was opened or mounted.
        assert_eq!(mappers(), 0);
        assert_eq!(mounted(), Vec::<String>::new());
    },
};

pub const UNMOUNT_REFUSES_WHAT_IT_DOES_NOT_STOP: Test = Test {
    name: "unmount_refuses_what_it_does_not_stop",
    disks: DISKS,
    run: || {
        applied_and_closed();
        assert_ok(&mount(), "mount");

        run("swapon", &["/dev/vg0/swap"]);
        assert_failed(&unmount(), "which unmount does not turn off");
        assert_eq!(mounted().len(), 6, "unmount changed something");
        run("swapoff", &["/dev/vg0/swap"]);

        fs::create_dir_all("/mnt/target/mnt").unwrap();
        run("mount", &["-t", "tmpfs", "tmpfs", "/mnt/target/mnt"]);
        assert_failed(&unmount(), "but not from the declaration");
        assert_eq!(mounted().len(), 7, "unmount changed something");
        run("umount", &["/mnt/target/mnt"]);

        assert_ok(&unmount(), "unmount");
        assert_eq!(mappers(), 0);
    },
};

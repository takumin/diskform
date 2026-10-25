//! `mount` and `unmount` before they read any device (ADR 0004, ADR 0019).

use std::path::{Path, PathBuf};

use diskform::change::{Activate, Change};
use diskform::load;
use diskform::model::Format;
use diskform::mount::unmount;

use crate::fake::*;
use crate::{run, write};

#[test]
fn the_root_is_required() {
    let file = write("yaml", "version: 1\n");
    for command in ["mount", "unmount"] {
        let out = run(&[command], &file);
        assert!(!out.success, "{command} ran without --root");
        assert!(out.stderr.contains("--root"), "{}", out.stderr);
    }
}

/// Stops the test at any change, since unmount has nothing to do here.
struct Unchanging;

impl Change for Unchanging {
    fn deactivate_volume_group(&self, name: &str) -> Result<(), String> {
        panic!("deactivated {name}")
    }
    fn close_luks(&self, name: &str) -> Result<(), String> {
        panic!("closed {name}")
    }
    fn wipe(&self, device: &Path) -> Result<(), String> {
        panic!("wiped {}", device.display())
    }
    fn forget_partitions(&self, disk: &Path) -> Result<(), String> {
        panic!("forgot the partitions of {}", disk.display())
    }
}

impl Activate for Unchanging {
    fn open_luks(&self, _: &Path, _: &Path, name: &str) -> Result<(), String> {
        panic!("opened {name}")
    }
    fn activate_volume_group(&self, name: &str) -> Result<(), String> {
        panic!("activated {name}")
    }
    fn mount(&self, _: Format, _: &[PathBuf], _: &[String], target: &Path) -> Result<(), String> {
        panic!("mounted {}", target.display())
    }
    fn unmount(&self, target: &Path) -> Result<(), String> {
        panic!("unmounted {}", target.display())
    }
}

#[test]
fn unmount_without_lvm_needs_no_lvm2() {
    let file = write(
        "yaml",
        "version: 1\n\
         disk:\n  sys0:\n    match: {path: /dev/sda}\n    table: gpt\n    partitions:\n    \
         - {name: root, size: 100%, label: root}\n\
         filesystem:\n  root: {device: disk.sys0.root, format: ext4, label: root, mount: /}\n",
    );
    let decl = load::load(&file).unwrap();
    let sys = Fake {
        mount_table: Some(Vec::new()),
        without_lvm2: true,
        ..Fake::default().disk("/dev/sda", 64 * GIB)
    };
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let unmounted = unmount(&decl, &sys, &Unchanging, root, &mut |_| {}, &mut |_| {});
    assert_eq!(unmounted.map_err(|e| format!("{e:?}")), Ok(false));
}

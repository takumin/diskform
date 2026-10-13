//! Running a test inside the VM, as root, against its blank disks.

use std::env;
use std::panic;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Output};

use crate::Test;

pub fn main(tests: &[Test], name: &str) -> ExitCode {
    let Some(test) = tests.iter().find(|t| t.name == name) else {
        println!("no test is named `{name}`");
        return ExitCode::FAILURE;
    };
    match panic::catch_unwind(test.run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// The disk of the VM with `serial`, by the name that udev gives it.
pub fn disk(serial: &str) -> String {
    format!("/dev/disk/by-id/virtio-{serial}")
}

pub fn partition(serial: &str, number: u32) -> String {
    format!("/dev/disk/by-id/virtio-{serial}-part{number}")
}

/// Runs a command that sets up or inspects the disks, and returns its
/// standard output. A failure fails the test.
pub fn run(program: &str, args: &[&str]) -> String {
    println!("$ {program} {}", args.join(" "));
    let out = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run `{program}`: {e}"));
    assert!(
        out.status.success(),
        "`{program}` failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Runs diskform, and shows its output in the log of the test.
pub fn diskform(args: &[&str]) -> Output {
    let exe = env::current_exe().expect("the path of the test");
    let diskform: PathBuf = exe.with_file_name("diskform");
    println!("$ diskform {}", args.join(" "));
    let out = Command::new(diskform)
        .args(args)
        .output()
        .expect("cannot run diskform");
    print!("{}", String::from_utf8_lossy(&out.stdout));
    print!("{}", String::from_utf8_lossy(&out.stderr));
    println!("(diskform exited with {})", out.status);
    out
}

/// The number of device-mapper devices, such as LUKS mappings and active
/// logical volumes.
pub fn mappers() -> usize {
    std::fs::read_dir("/sys/class/block")
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("dm-")
        })
        .count()
}

/// The types of the signatures that wipefs finds on a device.
pub fn signatures(device: &str) -> Vec<String> {
    run(
        "wipefs",
        &["--no-act", "--noheadings", "--output", "TYPE", device],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

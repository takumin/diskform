//! Apply-layer acceptance tests (AGENTS.md 5, ADR 0017).
//!
//! Each test boots its own VM with blank disks, and runs inside it as root
//! against those disks. This binary does both: on the host it boots a VM
//! per test, and in the VM, given `--guest NAME`, it runs that test.
//!
//! The tests run only when `DISKFORM_APPLY_VM` names a directory prepared
//! by `tests/apply/fetch-vm.sh`. Otherwise this binary says that it skipped
//! them, so that `cargo test` does not report them as passed.

mod apply;
mod destroy;
mod example;
mod guest;
mod mount;
mod vm;

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

pub const MIB: u64 = 1 << 20;
pub const GIB: u64 = 1 << 30;

pub struct Test {
    pub name: &'static str,
    /// The serial and the size of each blank disk of the VM.
    pub disks: &'static [(&'static str, u64)],
    pub run: fn(),
}

const TESTS: &[Test] = &[
    apply::CREATES_AND_KEEPS_THE_EXAMPLE,
    apply::REFUSES_A_DISK_WITH_A_SIGNATURE,
    apply::STOPS_AT_WHAT_EARLIER_STORAGE_LEFT,
    mount::MOUNTS_AND_UNMOUNTS_THE_EXAMPLE,
    mount::REFUSES_STORAGE_UNLIKE_THE_DECLARATION,
    mount::UNMOUNT_REFUSES_WHAT_IT_DOES_NOT_STOP,
    destroy::TEARS_DOWN_THE_EXAMPLE,
    destroy::REFUSES_A_MOUNTED_BTRFS,
    destroy::REFUSES_AN_ACTIVE_SWAP,
    destroy::REFUSES_A_VOLUME_GROUP_ON_AN_UNDECLARED_DISK,
];

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if let [flag, name] = args.as_slice() {
        if flag == "--guest" {
            return guest::main(TESTS, name);
        }
    }

    let Some(dir) = env::var_os("DISKFORM_APPLY_VM") else {
        println!(
            "skipped the apply-layer tests: DISKFORM_APPLY_VM is not set (see \
             tests/apply/fetch-vm.sh)"
        );
        return ExitCode::SUCCESS;
    };
    // Like libtest, the arguments that are not options filter the tests.
    let filters: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let tests: Vec<&Test> = TESTS
        .iter()
        .filter(|t| filters.is_empty() || filters.iter().any(|f| t.name.contains(f.as_str())))
        .collect();

    let vm = match vm::Vm::new(PathBuf::from(dir)) {
        Ok(vm) => vm,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("running {} apply-layer tests", tests.len());
    let mut failed = Vec::new();
    for test in tests {
        match vm.run(test) {
            Ok(()) => println!("test {} ... ok", test.name),
            Err(e) => {
                println!("test {} ... FAILED\n{e}", test.name);
                failed.push(test.name);
            }
        }
    }
    if failed.is_empty() {
        println!("all apply-layer tests passed");
        ExitCode::SUCCESS
    } else {
        println!("failed: {}", failed.join(", "));
        ExitCode::FAILURE
    }
}

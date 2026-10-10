//! Changing the storage of the machine (ADR 0016). Only `destroy` changes
//! devices, through `Change`, so that its order and its handling of
//! failures can be tested against a fake machine.

use std::path::Path;
use std::process::Command;

use crate::system::Host;

pub trait Change {
    /// Deactivates every logical volume of a volume group.
    fn deactivate_volume_group(&self, name: &str) -> Result<(), String>;
    /// Closes an open LUKS mapping by its device-mapper name.
    fn close_luks(&self, name: &str) -> Result<(), String>;
    /// Erases every signature that blkid finds on a device.
    fn wipe(&self, device: &Path) -> Result<(), String>;
    /// Has the kernel forget the partitions of a disk.
    fn forget_partitions(&self, disk: &Path) -> Result<(), String>;
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run `{program}`: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

impl Change for Host {
    fn deactivate_volume_group(&self, name: &str) -> Result<(), String> {
        run("vgchange", &["--activate", "n", name])
    }

    fn close_luks(&self, name: &str) -> Result<(), String> {
        run("cryptsetup", &["close", name])
    }

    fn wipe(&self, device: &Path) -> Result<(), String> {
        // Without --force, wipefs opens the device exclusively, so the
        // kernel refuses a device in use.
        run("wipefs", &["--all", &device.to_string_lossy()])
    }

    fn forget_partitions(&self, disk: &Path) -> Result<(), String> {
        run("partx", &["--delete", &disk.to_string_lossy()])
    }
}

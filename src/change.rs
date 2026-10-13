//! Changing the storage of the machine. `destroy` tears storage down
//! through `Change` (ADR 0016), so that its order and its handling of
//! failures can be tested against a fake machine. `mount` and `unmount`
//! put storage to use and stop using it through `Activate` (ADR 0019), and
//! `apply` builds storage through `Create` (ADR 0018). None of them checks
//! what it did; the callers read the result back through `System`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::layout::LUKS_DATA_OFFSET;
use crate::model::{BtrfsProfile, Format, Table};
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

/// Puts storage to use and stops using it (ADR 0019), besides tearing it
/// down as `Change` does.
pub trait Activate: Change {
    fn open_luks(&self, device: &Path, keyfile: &Path, name: &str) -> Result<(), String>;
    fn activate_volume_group(&self, name: &str) -> Result<(), String>;
    /// Mounts a filesystem on the existing directory `target`.
    fn mount(
        &self,
        format: Format,
        devices: &[PathBuf],
        options: &[String],
        target: &Path,
    ) -> Result<(), String>;
    fn unmount(&self, target: &Path) -> Result<(), String>;
}

/// Builds storage for apply (ADR 0018). Each method makes one thing, and
/// returns after udev has handled the events it caused, so that the next
/// one finds the devices in place.
pub trait Create: Activate {
    fn create_table(&self, disk: &Path, table: Table) -> Result<(), String>;
    /// Appends partition `number`, with its start and size in logical
    /// sectors.
    fn create_partition(
        &self,
        disk: &Path,
        number: usize,
        start: u64,
        sectors: u64,
        type_guid: &str,
        label: &str,
    ) -> Result<(), String>;
    fn format_luks(&self, device: &Path, keyfile: &Path) -> Result<(), String>;
    fn create_physical_volume(&self, device: &Path) -> Result<(), String>;
    fn create_volume_group(&self, name: &str, devices: &[PathBuf]) -> Result<(), String>;
    /// Creates and activates a logical volume of `size` bytes.
    fn create_logical_volume(&self, vg: &str, name: &str, size: u64) -> Result<(), String>;
    fn create_filesystem(
        &self,
        format: Format,
        devices: &[PathBuf],
        label: &str,
        profiles: Option<(BtrfsProfile, BtrfsProfile)>,
    ) -> Result<(), String>;
    fn create_swap(&self, device: &Path, label: &str) -> Result<(), String>;
    /// Creates a subvolume at `path` in a mounted btrfs.
    fn create_subvolume(&self, dir: &Path, path: &str) -> Result<(), String>;
}

/// Runs a command with `input` on its standard input. Without input, the
/// standard input is empty, so that a command that asks a question fails
/// instead of waiting.
fn run_with(program: &str, args: &[&str], input: &str) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run `{program}`: {e}"))?;
    let written = child
        .stdin
        .take()
        .expect("the standard input is piped")
        .write_all(input.as_bytes());
    let out = child
        .wait_with_output()
        .map_err(|e| format!("cannot run `{program}`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    written.map_err(|e| format!("cannot write to `{program}`: {e}"))
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    run_with(program, args, "")
}

/// Waits until udev has handled the events of a change.
fn settle() -> Result<(), String> {
    run("udevadm", &["settle"])
}

fn path(p: &Path) -> &str {
    p.to_str().expect("device paths are valid UTF-8")
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
        run("wipefs", &["--all", path(device)])
    }

    fn forget_partitions(&self, disk: &Path) -> Result<(), String> {
        run("partx", &["--delete", path(disk)])
    }
}

impl Activate for Host {
    fn open_luks(&self, device: &Path, keyfile: &Path, name: &str) -> Result<(), String> {
        let args = [
            "open",
            "--type",
            "luks2",
            "--key-file",
            path(keyfile),
            path(device),
            name,
        ];
        run("cryptsetup", &args)?;
        settle()
    }

    fn activate_volume_group(&self, name: &str) -> Result<(), String> {
        run("vgchange", &["--activate", "y", name])?;
        settle()
    }

    fn mount(
        &self,
        format: Format,
        devices: &[PathBuf],
        options: &[String],
        target: &Path,
    ) -> Result<(), String> {
        let mut options = options.to_vec();
        // Naming every device of a btrfs does not depend on udev having
        // registered them with the kernel.
        if devices.len() > 1 {
            options.extend(devices.iter().map(|d| format!("device={}", path(d))));
        }
        let first = devices.first().ok_or("a filesystem has a device")?;
        let format = format.to_string();
        let options = options.join(",");
        let mut args = vec!["-t", format.as_str()];
        if !options.is_empty() {
            args.extend(["-o", options.as_str()]);
        }
        args.extend([path(first), path(target)]);
        run("mount", &args)
    }

    fn unmount(&self, target: &Path) -> Result<(), String> {
        run("umount", &[path(target)])
    }
}

impl Create for Host {
    fn create_table(&self, disk: &Path, table: Table) -> Result<(), String> {
        // sfdisk wipes nothing; apply has checked that the disk is empty.
        let script = format!("label: {table}\ntable-length: 128\n");
        let args = ["--wipe", "never", "--wipe-partitions", "never", path(disk)];
        run_with("sfdisk", &args, &script)?;
        settle()
    }

    fn create_partition(
        &self,
        disk: &Path,
        number: usize,
        start: u64,
        sectors: u64,
        type_guid: &str,
        label: &str,
    ) -> Result<(), String> {
        let script = format!("start={start}, size={sectors}, type={type_guid}\n");
        let args = ["--append", "--wipe-partitions", "never", path(disk)];
        run_with("sfdisk", &args, &script)?;
        settle()?;
        // ADR 0008: the empty label means no label.
        if !label.is_empty() {
            // As an argument, the label needs none of the quoting of a script.
            let number = number.to_string();
            run("sfdisk", &["--part-label", path(disk), &number, label])?;
            settle()?;
        }
        Ok(())
    }

    fn format_luks(&self, device: &Path, keyfile: &Path) -> Result<(), String> {
        // ADR 0009: every parameter is given, even where it is the default.
        let offset = (LUKS_DATA_OFFSET / 512).to_string();
        run(
            "cryptsetup",
            &[
                "luksFormat",
                "--batch-mode",
                "--type",
                "luks2",
                "--cipher",
                "aes-xts-plain64",
                "--key-size",
                "512",
                "--pbkdf",
                "argon2id",
                "--offset",
                &offset,
                "--key-file",
                path(keyfile),
                path(device),
            ],
        )?;
        settle()
    }

    fn create_physical_volume(&self, device: &Path) -> Result<(), String> {
        // ADR 0010.
        run("pvcreate", &["--dataalignment", "1m", path(device)])?;
        settle()
    }

    fn create_volume_group(&self, name: &str, devices: &[PathBuf]) -> Result<(), String> {
        // ADR 0010.
        let mut args = vec!["--physicalextentsize", "4m", name];
        args.extend(devices.iter().map(|d| path(d)));
        run("vgcreate", &args)?;
        settle()
    }

    fn create_logical_volume(&self, vg: &str, name: &str, size: u64) -> Result<(), String> {
        // lvcreate neither wipes signatures nor zeroes the start of the
        // volume, so that apply finds what is there instead of writing over
        // it (ADR 0018).
        let size = format!("{size}b");
        let args = [
            "--name",
            name,
            "--size",
            &size,
            "--wipesignatures",
            "n",
            "--zero",
            "n",
            vg,
        ];
        run("lvcreate", &args)?;
        settle()
    }

    fn create_filesystem(
        &self,
        format: Format,
        devices: &[PathBuf],
        label: &str,
        profiles: Option<(BtrfsProfile, BtrfsProfile)>,
    ) -> Result<(), String> {
        let (program, label_option) = match format {
            Format::Vfat => ("mkfs.fat", "-n"),
            Format::Ext4 => ("mkfs.ext4", "-L"),
            Format::Xfs => ("mkfs.xfs", "-L"),
            Format::Btrfs => ("mkfs.btrfs", "-L"),
        };
        let mut args: Vec<String> = Vec::new();
        if format != Format::Vfat {
            args.push("-q".to_owned());
        }
        if !label.is_empty() {
            args.extend([label_option.to_owned(), label.to_owned()]);
        }
        if let Some((data, metadata)) = profiles {
            args.extend(["-d".to_owned(), data.to_string()]);
            args.extend(["-m".to_owned(), metadata.to_string()]);
        }
        args.extend(devices.iter().map(|d| path(d).to_owned()));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run(program, &args)?;
        settle()
    }

    fn create_swap(&self, device: &Path, label: &str) -> Result<(), String> {
        let mut args = Vec::new();
        if !label.is_empty() {
            args.extend(["-L", label]);
        }
        args.push(path(device));
        run("mkswap", &args)?;
        settle()
    }

    fn create_subvolume(&self, dir: &Path, subvolume: &str) -> Result<(), String> {
        run(
            "btrfs",
            &["subvolume", "create", path(&dir.join(subvolume))],
        )
    }
}

//! Reading the state of the machine. `plan` reads every device state
//! through `System`, so that plan-layer tests can supply a fake machine.
//! Nothing here changes a device.

use std::fs;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Disk,
    Partition,
    DeviceMapper,
    Md,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDevice {
    /// The canonical path, such as `/dev/sda`.
    pub path: PathBuf,
    pub kind: DeviceKind,
    pub size: u64,
    pub logical_sector: u64,
    pub model: Option<String>,
    pub serial: Option<String>,
}

/// What is already on a whole disk. The disk is empty when every list is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskState {
    /// Partition tables and signatures that blkid finds on the disk itself.
    pub signatures: Vec<String>,
    /// Partitions that the kernel knows on the disk.
    pub partitions: Vec<String>,
    /// Devices built on the disk itself (`/sys/block/*/holders`).
    pub holders: Vec<String>,
}

impl DiskState {
    pub fn is_empty(&self) -> bool {
        self.signatures.is_empty() && self.partitions.is_empty() && self.holders.is_empty()
    }
}

pub trait System {
    /// The entry names in `dir`; empty if `dir` does not exist.
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<String>>;
    /// Resolves every symbolic link in `path`.
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    /// The block device at a canonical path; `None` if it is not one.
    fn block_device(&self, path: &Path) -> io::Result<Option<BlockDevice>>;
    fn disk_state(&self, disk: &BlockDevice) -> Result<DiskState, String>;
    /// Checks that a key file can be used (ADR 0009).
    fn check_keyfile(&self, path: &Path) -> Result<(), String>;
    /// The names of the volume groups on this machine.
    fn volume_groups(&self) -> Result<Vec<String>, String>;
    /// The names of the device-mapper devices on this machine.
    fn mapper_names(&self) -> Result<Vec<String>, String>;
}

/// The machine diskform runs on.
pub struct Host;

const SYS_BLOCK: &str = "/sys/class/block";

fn read_trimmed(path: &Path) -> Option<String> {
    let s = fs::read_to_string(path).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

/// The serial from the udev database, which also covers SATA disks whose
/// serial is not in sysfs.
fn udev_serial(sys: &Path) -> Option<String> {
    let dev = read_trimmed(&sys.join("dev"))?;
    let data = fs::read_to_string(format!("/run/udev/data/b{dev}")).ok()?;
    data.lines()
        .find_map(|l| l.strip_prefix("E:ID_SERIAL_SHORT="))
        .map(str::to_owned)
}

fn names_in(dir: &Path) -> io::Result<Vec<String>> {
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| Ok(e?.file_name().to_string_lossy().into_owned()))
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

fn run(program: &str, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(program)
        .args(args)
        .output()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => format!("`{program}` was not found"),
            _ => format!("cannot run `{program}`: {e}"),
        })
}

impl System for Host {
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
        names_in(dir)
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }

    fn block_device(&self, path: &Path) -> io::Result<Option<BlockDevice>> {
        if !fs::metadata(path)?.file_type().is_block_device() {
            return Ok(None);
        }
        let Some(name) = path.file_name() else {
            return Ok(None);
        };
        let sys = Path::new(SYS_BLOCK).join(name);
        let kind = if sys.join("partition").exists() {
            DeviceKind::Partition
        } else if sys.join("dm").exists() {
            DeviceKind::DeviceMapper
        } else if sys.join("md").exists() {
            DeviceKind::Md
        } else {
            DeviceKind::Disk
        };
        let number = |file: &str| -> io::Result<u64> {
            let s = fs::read_to_string(sys.join(file))?;
            s.trim().parse().map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}/{file}: {e}", sys.display()),
                )
            })
        };
        // sysfs counts the size in 512-byte units whatever the sector size.
        let size = number("size")? * 512;
        let logical_sector = match kind {
            DeviceKind::Disk => number("queue/logical_block_size")?,
            _ => 512,
        };
        Ok(Some(BlockDevice {
            path: path.to_owned(),
            kind,
            size,
            logical_sector,
            model: read_trimmed(&sys.join("device/model")),
            serial: read_trimmed(&sys.join("device/serial")).or_else(|| udev_serial(&sys)),
        }))
    }

    fn disk_state(&self, disk: &BlockDevice) -> Result<DiskState, String> {
        let name = disk.path.file_name().unwrap_or_default();
        let sys = Path::new(SYS_BLOCK).join(name);
        let error = |e: io::Error| format!("cannot read {}: {e}", sys.display());
        let mut partitions: Vec<String> = names_in(&sys)
            .map_err(error)?
            .into_iter()
            .filter(|n| sys.join(n).join("partition").exists())
            .collect();
        partitions.sort();
        let mut holders = names_in(&sys.join("holders")).map_err(error)?;
        holders.sort();

        // Low-level probing reads the disk itself rather than the cache.
        let device = disk.path.to_string_lossy();
        let out = run("blkid", &["--probe", "--output", "export", &device])?;
        let signatures = match out.status.code() {
            Some(0) => String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| match l.split_once('=')? {
                    ("PTTYPE", v) => Some(format!("{v} partition table")),
                    ("TYPE", v) => Some(format!("{v} signature")),
                    _ => None,
                })
                .collect(),
            // Nothing was found.
            Some(2) => Vec::new(),
            // Several signatures that blkid cannot choose between.
            Some(8) => vec!["several ambivalent signatures".to_owned()],
            _ => {
                return Err(format!(
                    "blkid cannot probe {device}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        };
        Ok(DiskState {
            signatures,
            partitions,
            holders,
        })
    }

    fn check_keyfile(&self, path: &Path) -> Result<(), String> {
        let meta = fs::metadata(path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => "does not exist".to_owned(),
            _ => format!("cannot be read: {e}"),
        })?;
        if !meta.is_file() {
            return Err("is not a regular file".to_owned());
        }
        if meta.len() == 0 {
            return Err("is empty".to_owned());
        }
        fs::File::open(path).map_err(|e| format!("cannot be read: {e}"))?;
        Ok(())
    }

    fn volume_groups(&self) -> Result<Vec<String>, String> {
        let out = run("vgs", &["--noheadings", "--options", "vg_name"])
            .map_err(|e| format!("{e}; LVM2 is needed to check volume group names"))?;
        if !out.status.success() {
            return Err(format!(
                "vgs failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn mapper_names(&self) -> Result<Vec<String>, String> {
        let error = |e: io::Error| format!("cannot read {SYS_BLOCK}: {e}");
        let mut names = Vec::new();
        for device in names_in(Path::new(SYS_BLOCK)).map_err(error)? {
            if let Some(name) = read_trimmed(&Path::new(SYS_BLOCK).join(device).join("dm/name")) {
                names.push(name);
            }
        }
        Ok(names)
    }
}

//! Reading the state of the machine. `plan` reads every device state
//! through `System`, so that plan-layer tests can supply a fake machine.
//! Nothing here changes a device.

use std::collections::{BTreeMap, BTreeSet};
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

/// What blkid finds on a device by low-level probing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    /// The type of the signature, such as `ext4` or `crypto_LUKS`.
    pub kind: Option<String>,
    pub version: Option<String>,
    pub label: Option<String>,
    pub uuid: Option<String>,
    /// The type of the partition table, such as `gpt`.
    pub table: Option<String>,
}

/// An entry of the partition table on a disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionEntry {
    pub number: u32,
    /// The start and the size in bytes.
    pub start: u64,
    pub size: u64,
    /// The GPT partition type GUID, in lower case.
    pub type_guid: String,
    /// The GPT partition name (PARTLABEL).
    pub name: String,
    /// The kernel's device for the partition.
    pub device: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionTable {
    /// The type of the table, such as `gpt`.
    pub kind: String,
    pub partitions: Vec<PartitionEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeGroupState {
    pub extent_size: u64,
    /// The canonical paths of the physical volumes.
    pub devices: BTreeSet<PathBuf>,
    pub volumes: BTreeMap<String, LogicalVolumeState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalVolumeState {
    pub size: u64,
    /// The canonical path of the device; `None` if the volume is not active.
    pub device: Option<PathBuf>,
}

/// A mounted btrfs filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtrfsState {
    /// The canonical paths of the devices.
    pub devices: BTreeSet<PathBuf>,
    /// The profiles that the data and the metadata are stored in, such as
    /// `raid1`. There are several while a conversion is in progress.
    pub data_profiles: BTreeSet<String>,
    pub metadata_profiles: BTreeSet<String>,
    /// The paths of the subvolumes from the top of the filesystem.
    pub subvolumes: BTreeSet<String>,
}

pub trait System {
    /// The entry names in `dir`; empty if `dir` does not exist.
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<String>>;
    /// Resolves every symbolic link in `path`.
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    /// The block device at a canonical path; `None` if it is not one.
    fn block_device(&self, path: &Path) -> io::Result<Option<BlockDevice>>;
    fn disk_state(&self, disk: &BlockDevice) -> Result<DiskState, String>;
    /// The partition table on a disk; `None` if it has none. An error if
    /// the kernel's partitions differ from the table (ADR 0015).
    fn partition_table(&self, disk: &BlockDevice) -> Result<Option<PartitionTable>, String>;
    /// The signature on a device.
    fn probe(&self, device: &Path) -> Result<Probe, String>;
    /// Checks that a key file can be used (ADR 0009).
    fn check_keyfile(&self, path: &Path) -> Result<(), String>;
    /// Whether the key file opens a keyslot of the LUKS device, without
    /// creating a mapping (ADR 0011).
    fn test_keyfile(&self, device: &Path, keyfile: &Path) -> Result<bool, String>;
    /// The canonical path of the open mapping of a LUKS device; `None` if
    /// it is closed.
    fn luks_mapping(&self, device: &Path) -> Result<Option<PathBuf>, String>;
    /// The volume groups on this machine, by name.
    fn volume_groups(&self) -> Result<BTreeMap<String, VolumeGroupState>, String>;
    /// The btrfs filesystem with this UUID; `None` if it is not mounted.
    fn btrfs(&self, uuid: &str) -> Result<Option<BtrfsState>, String>;
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

/// Decodes the `\xHH` escapes that blkid and partx write.
fn decode_hex(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'x') {
            if let Some(b) = s
                .get(i + 2..i + 4)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(b);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The values that blkid finds by low-level probing, which reads the device
/// itself rather than the cache; empty if nothing was found.
fn blkid(device: &Path) -> Result<BTreeMap<String, String>, String> {
    let name = device.to_string_lossy();
    let out = run("blkid", &["--probe", "--output", "udev", &name])?;
    match out.status.code() {
        Some(0) => Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()),
        // Nothing was found.
        Some(2) => Ok(BTreeMap::new()),
        Some(8) => Err(format!(
            "{name} has several signatures that blkid cannot choose between"
        )),
        _ => Err(format!(
            "blkid cannot probe {name}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

fn failed(program: &str, out: &std::process::Output) -> String {
    format!(
        "{program} failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

/// The rows of an LVM report in JSON (`pvs`, `vgs` or `lvs`), sizes in bytes.
fn lvm_report(program: &str, fields: &str) -> Result<Vec<BTreeMap<String, String>>, String> {
    let args = [
        "--reportformat",
        "json",
        "--units",
        "b",
        "--nosuffix",
        "--options",
        fields,
    ];
    let out =
        run(program, &args).map_err(|e| format!("{e}; LVM2 is needed to read volume groups"))?;
    if !out.status.success() {
        return Err(failed(program, &out));
    }
    let invalid = |e: &dyn std::fmt::Display| format!("cannot parse the output of {program}: {e}");
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| invalid(&e))?;
    let key = program.trim_end_matches('s');
    let rows = report["report"][0][key]
        .as_array()
        .ok_or_else(|| invalid(&format!("no `{key}` list")))?;
    rows.iter()
        .map(|row| {
            row.as_object()
                .ok_or_else(|| invalid(&"a row is not an object"))?
                .iter()
                .map(|(k, v)| match v.as_str() {
                    Some(v) => Ok((k.clone(), v.to_owned())),
                    None => Err(invalid(&format!("`{k}` is not a string"))),
                })
                .collect()
        })
        .collect()
}

/// Decodes the octal escapes of `/proc/self/mountinfo`.
fn decode_octal(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if let Some(b) = s
                .get(i + 1..i + 4)
                .and_then(|o| u8::from_str_radix(o, 8).ok())
            {
                out.push(b);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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

        let values = blkid(&disk.path)?;
        let signatures = values
            .get("ID_PART_TABLE_TYPE")
            .map(|t| format!("{t} partition table"))
            .into_iter()
            .chain(values.get("ID_FS_TYPE").map(|t| format!("{t} signature")))
            .collect();
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

    fn partition_table(&self, disk: &BlockDevice) -> Result<Option<PartitionTable>, String> {
        let Some(kind) = self.probe(&disk.path)?.table else {
            return Ok(None);
        };
        let device = disk.path.to_string_lossy();
        // partx reads the table on the disk, not the kernel's partitions.
        let fields = "NR,START,SECTORS,TYPE,NAME";
        let out = run(
            "partx",
            &["--raw", "--noheadings", "--output", fields, &device],
        )?;
        if !out.status.success() {
            return Err(failed("partx", &out));
        }
        let invalid = |line: &str| format!("cannot parse the output of partx: `{line}`");
        let mut partitions = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            // The name is last, so that its escaped spaces do not matter.
            let f: Vec<&str> = line.splitn(5, ' ').collect();
            let number = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok());
            let (Some(nr), Some(start), Some(sectors), Some(guid)) =
                (number(0), number(1), number(2), f.get(3))
            else {
                return Err(invalid(line));
            };
            partitions.push(PartitionEntry {
                number: u32::try_from(nr).map_err(|_| invalid(line))?,
                // libblkid counts in 512-byte sectors whatever the sector size.
                start: start * 512,
                size: sectors * 512,
                type_guid: guid.to_lowercase(),
                name: decode_hex(f.get(4).unwrap_or(&"")),
                device: PathBuf::new(),
            });
        }

        let name = disk.path.file_name().unwrap_or_default();
        let sys = Path::new(SYS_BLOCK).join(name);
        let error = |e: io::Error| format!("cannot read {}: {e}", sys.display());
        let mut kernel = BTreeMap::new();
        for part in names_in(&sys).map_err(error)? {
            let dir = sys.join(&part);
            let Some(number) = read_trimmed(&dir.join("partition")) else {
                continue;
            };
            let value =
                |file: &str| read_trimmed(&dir.join(file)).and_then(|v| v.parse::<u64>().ok());
            let (Ok(number), Some(start), Some(size)) =
                (number.parse::<u32>(), value("start"), value("size"))
            else {
                return Err(format!("cannot read the partition {}", dir.display()));
            };
            kernel.insert(
                number,
                (start * 512, size * 512, Path::new("/dev").join(&part)),
            );
        }
        let differ = || {
            format!(
                "the partitions that the kernel knows on {device} differ from the partition \
                 table on it; have the kernel re-read the table, for example with \
                 `partx --update {device}`"
            )
        };
        if kernel.len() != partitions.len() {
            return Err(differ());
        }
        for p in &mut partitions {
            match kernel.remove(&p.number) {
                Some((start, size, device)) if start == p.start && size == p.size => {
                    p.device = device;
                }
                _ => return Err(differ()),
            }
        }
        Ok(Some(PartitionTable { kind, partitions }))
    }

    fn probe(&self, device: &Path) -> Result<Probe, String> {
        let mut values = blkid(device)?;
        let mut take = |key: &str| values.remove(key);
        Ok(Probe {
            kind: take("ID_FS_TYPE"),
            version: take("ID_FS_VERSION"),
            label: take("ID_FS_LABEL_ENC").map(|l| decode_hex(&l)),
            uuid: take("ID_FS_UUID_ENC").map(|u| decode_hex(&u)),
            table: take("ID_PART_TABLE_TYPE"),
        })
    }

    fn test_keyfile(&self, device: &Path, keyfile: &Path) -> Result<bool, String> {
        let device = device.to_string_lossy();
        let keyfile = keyfile.to_string_lossy();
        let args = ["open", "--test-passphrase", "--key-file", &keyfile, &device];
        let out = run("cryptsetup", &args)?;
        match out.status.code() {
            Some(0) => Ok(true),
            // No keyslot opens with the key.
            Some(2) => Ok(false),
            _ => Err(failed("cryptsetup", &out)),
        }
    }

    fn luks_mapping(&self, device: &Path) -> Result<Option<PathBuf>, String> {
        let name = device.file_name().unwrap_or_default();
        let holders = Path::new(SYS_BLOCK).join(name).join("holders");
        let mut mappings = Vec::new();
        for holder in
            names_in(&holders).map_err(|e| format!("cannot read {}: {e}", holders.display()))?
        {
            let uuid = read_trimmed(&Path::new(SYS_BLOCK).join(&holder).join("dm/uuid"));
            if uuid.is_some_and(|u| u.starts_with("CRYPT-LUKS")) {
                mappings.push(Path::new("/dev").join(holder));
            }
        }
        match <[_; 1]>::try_from(mappings) {
            Ok([mapping]) => Ok(Some(mapping)),
            Err(m) if m.is_empty() => Ok(None),
            Err(m) => Err(format!(
                "{} has several LUKS mappings: {}",
                device.display(),
                m.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    fn volume_groups(&self) -> Result<BTreeMap<String, VolumeGroupState>, String> {
        let number = |row: &BTreeMap<String, String>, key: &str| {
            row[key]
                .parse::<u64>()
                .map_err(|e| format!("cannot parse `{key}` of LVM: {e}"))
        };
        let mut groups = BTreeMap::new();
        for row in lvm_report("vgs", "vg_name,vg_extent_size")? {
            let state = VolumeGroupState {
                extent_size: number(&row, "vg_extent_size")?,
                devices: BTreeSet::new(),
                volumes: BTreeMap::new(),
            };
            groups.insert(row["vg_name"].clone(), state);
        }
        for row in lvm_report("pvs", "pv_name,vg_name")? {
            if let Some(vg) = groups.get_mut(&row["vg_name"]) {
                // A missing PV is named `[unknown]`; keep it as it is.
                let path = PathBuf::from(&row["pv_name"]);
                vg.devices.insert(fs::canonicalize(&path).unwrap_or(path));
            }
        }
        for row in lvm_report("lvs", "vg_name,lv_name,lv_size,lv_active,lv_dm_path")? {
            let size = number(&row, "lv_size")?;
            let device = (row["lv_active"] == "active")
                .then(|| fs::canonicalize(&row["lv_dm_path"]).ok())
                .flatten();
            if let Some(vg) = groups.get_mut(&row["vg_name"]) {
                vg.volumes
                    .insert(row["lv_name"].clone(), LogicalVolumeState { size, device });
            }
        }
        Ok(groups)
    }

    fn btrfs(&self, uuid: &str) -> Result<Option<BtrfsState>, String> {
        if uuid.is_empty() || !uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return Err(format!("`{uuid}` is not a btrfs UUID"));
        }
        // The directory exists while the filesystem is mounted.
        let dir = Path::new("/sys/fs/btrfs").join(uuid);
        if !dir.exists() {
            return Ok(None);
        }
        let error = |e: io::Error| format!("cannot read {}: {e}", dir.display());
        let devices: BTreeSet<PathBuf> = names_in(&dir.join("devices"))
            .map_err(error)?
            .into_iter()
            .map(|d| Path::new("/dev").join(d))
            .collect();
        let profiles = |kind: &str| -> Result<BTreeSet<String>, String> {
            let dir = dir.join("allocation").join(kind);
            Ok(names_in(&dir)
                .map_err(error)?
                .into_iter()
                .filter(|p| dir.join(p).is_dir())
                .collect())
        };

        let mountinfo = fs::read_to_string("/proc/self/mountinfo")
            .map_err(|e| format!("cannot read /proc/self/mountinfo: {e}"))?;
        let mount_point = mountinfo.lines().find_map(|line| {
            let (mount, fs) = line.split_once(" - ")?;
            let mut fs = fs.split(' ');
            let (Some("btrfs"), Some(source)) = (fs.next(), fs.next()) else {
                return None;
            };
            let source = fs::canonicalize(decode_octal(source)).ok()?;
            devices
                .contains(&source)
                .then(|| mount.split(' ').nth(4).map(decode_octal))
                .flatten()
        });
        let Some(mount_point) = mount_point else {
            return Ok(None);
        };
        let out = run("btrfs", &["subvolume", "list", &mount_point])?;
        if !out.status.success() {
            return Err(failed("btrfs", &out));
        }
        let subvolumes = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_once(" path ").map(|(_, p)| p))
            .map(|p| p.strip_prefix("<FS_TREE>/").unwrap_or(p).to_owned())
            .collect();
        Ok(Some(BtrfsState {
            devices,
            data_profiles: profiles("data")?,
            metadata_profiles: profiles("metadata")?,
            subvolumes,
        }))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_are_decoded() {
        // As blkid and partx write `a"b\c $dé` and `é`.
        assert_eq!(decode_hex(r"a\x22b\x5cc\x20\x24dé"), "a\"b\\c $dé");
        assert_eq!(decode_hex(r"\xc3\xa9"), "é");
        assert_eq!(decode_hex(r"a\xzz\x2"), r"a\xzz\x2");
        assert_eq!(decode_octal(r"/mnt/a\040b"), "/mnt/a b");
    }
}

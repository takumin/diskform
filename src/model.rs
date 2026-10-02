//! The declaration model (ADR 0005).
//!
//! Each scalar type checks its own syntax while it is read, so that a value
//! of the type is known to be well-formed. Rules that relate several values
//! are checked by `validate`.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;

use crate::size::{FixedSize, Size};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    #[expect(dead_code, reason = "read only by plan, which is not implemented yet")]
    pub version: Version,
    #[serde(default)]
    pub disk: BTreeMap<Name, Disk>,
    #[serde(default)]
    pub luks: BTreeMap<Name, Luks>,
    #[serde(default)]
    pub lvm: BTreeMap<Name, VolumeGroup>,
    #[serde(default)]
    pub filesystem: BTreeMap<Name, Filesystem>,
    #[serde(default)]
    pub swap: BTreeMap<Name, Swap>,
}

/// The declaration format version. Only `1` exists.
#[derive(Debug)]
pub struct Version;

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // `u64` rather than `serde_json::Value`: a string `"1"` must be rejected.
        match u64::deserialize(d)? {
            1 => Ok(Version),
            n => Err(serde::de::Error::custom(format!(
                "unsupported version {n}; the only supported version is 1"
            ))),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disk {
    #[serde(rename = "match")]
    #[expect(dead_code, reason = "read only by plan, which is not implemented yet")]
    pub matcher: Match,
    #[expect(dead_code, reason = "read only by plan, which is not implemented yet")]
    pub table: Table,
    pub partitions: Vec<Partition>,
}

/// Conditions that select the disk (ADR 0003).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(dead_code, reason = "read only by plan, which is not implemented yet")]
pub struct Match {
    pub path: DevicePath,
    pub min_size: Option<FixedSize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Table {
    Gpt,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Partition {
    pub name: Name,
    pub size: Size,
    #[serde(rename = "type")]
    #[expect(dead_code, reason = "read only by plan, which is not implemented yet")]
    pub kind: Option<PartitionType>,
    pub label: Option<Label>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PartitionType {
    Esp,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Luks {
    pub device: Ref,
    #[expect(dead_code, reason = "read only by plan, which is not implemented yet")]
    pub keyfile: AbsolutePath,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeGroup {
    pub devices: Vec<Ref>,
    pub volumes: BTreeMap<Name, LogicalVolume>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalVolume {
    pub size: Size,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filesystem {
    pub device: Option<Ref>,
    pub devices: Option<Vec<Ref>>,
    pub format: Format,
    pub label: Option<Label>,
    pub mount: Option<AbsolutePath>,
    pub mount_options: Option<Vec<String>>,
    pub btrfs: Option<BtrfsOptions>,
    pub subvolumes: Option<BTreeMap<Name, Subvolume>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Vfat,
    Ext4,
    Xfs,
    Btrfs,
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Format::Vfat => "vfat",
            Format::Ext4 => "ext4",
            Format::Xfs => "xfs",
            Format::Btrfs => "btrfs",
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BtrfsOptions {
    pub data_profile: BtrfsProfile,
    pub metadata_profile: BtrfsProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BtrfsProfile {
    Raid1,
}

impl BtrfsProfile {
    /// The number of devices the profile needs at least.
    pub fn min_devices(self) -> usize {
        match self {
            BtrfsProfile::Raid1 => 2,
        }
    }
}

impl fmt::Display for BtrfsProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BtrfsProfile::Raid1 => "raid1",
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subvolume {
    pub path: SubvolumePath,
    pub mount: Option<AbsolutePath>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Swap {
    pub device: Ref,
    pub label: Option<Label>,
}

/// A name of a declaration element: `[a-z0-9][a-z0-9_-]*`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct Name(String);

impl Name {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Name {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let mut bytes = s.bytes();
        let valid = bytes
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && bytes
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if valid {
            Ok(Name(s))
        } else {
            Err(format!(
                "invalid name `{s}`: a name must match [a-z0-9][a-z0-9_-]*"
            ))
        }
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A reference to an element that provides a block device.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(try_from = "String")]
pub enum Ref {
    Partition { disk: Name, partition: Name },
    Luks { name: Name },
    LogicalVolume { vg: Name, lv: Name },
}

impl TryFrom<String> for Ref {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let parts: Vec<&str> = s.split('.').collect();
        let name = |i: usize| -> Result<Name, String> {
            Name::try_from(parts[i].to_owned()).map_err(|_| {
                format!(
                    "invalid reference `{s}`: `{}` is not a valid name",
                    parts[i]
                )
            })
        };
        match (parts[0], parts.len()) {
            ("disk", 3) => Ok(Ref::Partition {
                disk: name(1)?,
                partition: name(2)?,
            }),
            ("disk", 2) => Err(format!(
                "invalid reference `{s}`: a disk cannot be referenced; reference one of its partitions"
            )),
            ("luks", 2) => Ok(Ref::Luks { name: name(1)? }),
            ("lvm", 3) => Ok(Ref::LogicalVolume {
                vg: name(1)?,
                lv: name(2)?,
            }),
            ("lvm", 2) => Err(format!(
                "invalid reference `{s}`: a volume group cannot be referenced; reference one of its volumes"
            )),
            ("filesystem" | "swap", _) => Err(format!(
                "invalid reference `{s}`: a {} does not provide a block device",
                parts[0]
            )),
            ("disk" | "luks" | "lvm", _) => Err(format!(
                "invalid reference `{s}`: expected disk.<disk>.<partition>, luks.<name> or lvm.<vg>.<volume>"
            )),
            _ => Err(format!(
                "invalid reference `{s}`: unknown kind `{}`; expected disk, luks or lvm",
                parts[0]
            )),
        }
    }
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ref::Partition { disk, partition } => write!(f, "disk.{disk}.{partition}"),
            Ref::Luks { name } => write!(f, "luks.{name}"),
            Ref::LogicalVolume { vg, lv } => write!(f, "lvm.{vg}.{lv}"),
        }
    }
}

/// A label written to a partition, a filesystem or swap. Never empty; the
/// length limit depends on where it is written and is checked by `validate`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Label(String);

impl Label {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Label {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        if s.is_empty() {
            return Err("a label must not be empty; omit `label` to write no label".to_owned());
        }
        if s.chars().any(char::is_control) {
            return Err(format!(
                "invalid label {s:?}: control characters are not allowed"
            ));
        }
        Ok(Label(s))
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Checks that `s` is an absolute path without empty, `.` or `..`
/// components and without a trailing slash, so that it has one spelling.
fn check_absolute(s: &str) -> Result<(), String> {
    if s == "/" {
        return Ok(());
    }
    let Some(rest) = s.strip_prefix('/') else {
        return Err(format!("invalid path `{s}`: must be absolute"));
    };
    check_components(s, rest)
}

fn check_components(s: &str, rest: &str) -> Result<(), String> {
    if s.contains('\0') {
        return Err(format!("invalid path {s:?}: must not contain NUL"));
    }
    if rest
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err(format!(
            "invalid path `{s}`: must not contain empty, `.` or `..` components or a trailing slash"
        ));
    }
    Ok(())
}

/// An absolute path in normal form, such as a mount point or a key file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub struct AbsolutePath(String);

impl AbsolutePath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AbsolutePath {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        check_absolute(&s)?;
        Ok(AbsolutePath(s))
    }
}

/// The `match.path` of a disk (ADR 0003): an absolute path under `/dev/`
/// whose last component may contain wildcards.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct DevicePath(String);

impl TryFrom<String> for DevicePath {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        check_absolute(&s)?;
        let Some((dir, file)) = s.rsplit_once('/').filter(|_| s.starts_with("/dev/")) else {
            return Err(format!("invalid device path `{s}`: must start with /dev/"));
        };
        if dir.contains(['*', '?', '[', ']']) {
            return Err(format!(
                "invalid device path `{s}`: wildcards are allowed only in the last component"
            ));
        }
        check_brackets(file).map_err(|e| format!("invalid device path `{s}`: {e}"))?;
        Ok(DevicePath(s))
    }
}

/// Checks that every `[` in a wildcard pattern opens a non-empty class that
/// is closed, and that no `]` appears outside a class.
fn check_brackets(pattern: &str) -> Result<(), &'static str> {
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '[' => {
                let mut class = String::new();
                loop {
                    match chars.next() {
                        Some(']') if !class.is_empty() && class != "!" => break,
                        Some('[') => return Err("`[` inside a bracket expression"),
                        Some(c) => class.push(c),
                        None => return Err("unclosed `[`"),
                    }
                }
            }
            ']' => return Err("unmatched `]`"),
            _ => {}
        }
    }
    Ok(())
}

/// The path of a subvolume inside its btrfs filesystem.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub struct SubvolumePath(String);

impl SubvolumePath {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SubvolumePath {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        if s.starts_with('/') {
            return Err(format!(
                "invalid subvolume path `{s}`: must be relative to the top of the filesystem"
            ));
        }
        check_components(&s, &s)?;
        Ok(SubvolumePath(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["a", "0", "sys0", "vg_0", "data-1"] {
            assert!(Name::try_from(ok.to_owned()).is_ok(), "{ok}");
        }
        for bad in ["", "A", "_a", "-a", "a.b", "a b", "é", "sys0/"] {
            assert!(Name::try_from(bad.to_owned()).is_err(), "{bad}");
        }
    }

    #[test]
    fn references() {
        let r = |s: &str| Ref::try_from(s.to_owned());
        let n = |s: &str| Name::try_from(s.to_owned()).unwrap();
        assert_eq!(
            r("disk.sys0.esp"),
            Ok(Ref::Partition {
                disk: n("sys0"),
                partition: n("esp")
            })
        );
        assert_eq!(
            r("luks.cryptsys"),
            Ok(Ref::Luks {
                name: n("cryptsys")
            })
        );
        assert_eq!(
            r("lvm.vg0.root"),
            Ok(Ref::LogicalVolume {
                vg: n("vg0"),
                lv: n("root")
            })
        );
        for (bad, why) in [
            ("disk.sys0", "a disk cannot be referenced"),
            ("lvm.vg0", "a volume group cannot be referenced"),
            ("filesystem.root", "does not provide a block device"),
            ("swap.main", "does not provide a block device"),
            ("luks.a.b", "expected disk"),
            ("disk.a.b.c", "expected disk"),
            ("md.a", "unknown kind"),
            ("luks.A", "not a valid name"),
            ("luks.", "not a valid name"),
            ("", "unknown kind"),
        ] {
            let e = r(bad).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
        }
        assert_eq!(r("lvm.vg0.root").unwrap().to_string(), "lvm.vg0.root");
    }

    #[test]
    fn absolute_paths() {
        for ok in ["/", "/boot/efi", "/srv/.snapshots", "/run/keys/sys.key"] {
            assert!(AbsolutePath::try_from(ok.to_owned()).is_ok(), "{ok}");
        }
        for bad in ["", "boot", "/boot/", "//boot", "/a/../b", "/a/./b", "/a\0"] {
            assert!(AbsolutePath::try_from(bad.to_owned()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn device_paths() {
        for ok in [
            "/dev/loop0",
            "/dev/disk/by-id/nvme-Samsung_SSD_980_PRO_1TB_*",
            "/dev/disk/by-path/pci-0000:00:17.0-ata-1",
            "/dev/sd?",
            "/dev/sd[ab]",
            "/dev/sd[!a]",
        ] {
            assert!(DevicePath::try_from(ok.to_owned()).is_ok(), "{ok}");
        }
        for bad in [
            "/dev",
            "/dev/",
            "/sys/block/sda",
            "dev/sda",
            "/dev/../etc/passwd",
            "/dev/disk/*/foo",
            "/dev/disk/by-[ip]d/foo",
            "/dev/sd[ab",
            "/dev/sd[]",
            "/dev/sd[!]",
            "/dev/sda]",
        ] {
            assert!(DevicePath::try_from(bad.to_owned()).is_err(), "{bad}");
        }
    }

    #[test]
    fn subvolume_paths() {
        for ok in ["@srv", "@snapshots", "a/b"] {
            assert!(SubvolumePath::try_from(ok.to_owned()).is_ok(), "{ok}");
        }
        for bad in ["", "/@srv", "@srv/", "a//b", "..", "a/../b", "."] {
            assert!(SubvolumePath::try_from(bad.to_owned()).is_err(), "{bad}");
        }
    }

    #[test]
    fn labels() {
        assert!(Label::try_from("EFI".to_owned()).is_ok());
        assert!(Label::try_from(String::new()).is_err());
        assert!(Label::try_from("a\nb".to_owned()).is_err());
    }
}

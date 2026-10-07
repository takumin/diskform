//! Size notation (ADR 0002).

use std::str::FromStr;

use serde::Deserialize;

/// A size of a partition or a logical volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum Size {
    /// A fixed size in bytes.
    Fixed(u64),
    /// A percentage (1 to 100) of the base amount defined in ADR 0002.
    Percent(u8),
}

/// A size that must not be a percentage, such as `match.min_size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct FixedSize(pub u64);

/// Binary units (ADR 0002) and decimal units (ADR 0014), with their sizes
/// in bytes. No suffix is a suffix of another, so the order does not matter.
const UNITS: [(&str, u64); 8] = [
    ("KiB", 1 << 10),
    ("MiB", 1 << 20),
    ("GiB", 1 << 30),
    ("TiB", 1 << 40),
    ("KB", 1_000),
    ("MB", 1_000_000),
    ("GB", 1_000_000_000),
    ("TB", 1_000_000_000_000),
];

/// Parses a positive decimal integer without sign or leading zeros.
fn parse_positive(digits: &str) -> Option<u64> {
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

impl FromStr for Size {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        if let Some(digits) = s.strip_suffix('%') {
            return match parse_positive(digits) {
                Some(n @ 1..=100) => Ok(Size::Percent(n as u8)),
                _ => Err(format!(
                    "invalid size `{s}`: a percentage must be an integer from 1 to 100"
                )),
            };
        }
        let invalid = || {
            format!(
                "invalid size `{s}`: expected a positive integer with KiB, MiB, GiB, TiB, \
                 KB, MB, GB or TB, \
                 or a percentage such as 50%"
            )
        };
        let (digits, unit) = UNITS
            .iter()
            .find_map(|(unit, bytes)| Some((s.strip_suffix(unit)?, *bytes)))
            .ok_or_else(invalid)?;
        let n = parse_positive(digits).ok_or_else(invalid)?;
        n.checked_mul(unit)
            .map(Size::Fixed)
            .ok_or_else(|| format!("invalid size `{s}`: too large"))
    }
}

impl TryFrom<String> for Size {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        s.parse()
    }
}

impl TryFrom<String> for FixedSize {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        match s.parse()? {
            Size::Fixed(bytes) => Ok(FixedSize(bytes)),
            Size::Percent(_) => Err(format!(
                "invalid size `{s}`: a percentage is not allowed here"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixed_sizes() {
        assert_eq!("512MiB".parse(), Ok(Size::Fixed(512 << 20)));
        assert_eq!("1GiB".parse(), Ok(Size::Fixed(1 << 30)));
        assert_eq!("4TiB".parse(), Ok(Size::Fixed(4 << 40)));
        assert_eq!("1KiB".parse(), Ok(Size::Fixed(1024)));
    }

    #[test]
    fn parses_decimal_sizes() {
        assert_eq!("1KB".parse(), Ok(Size::Fixed(1_000)));
        assert_eq!("512MB".parse(), Ok(Size::Fixed(512_000_000)));
        assert_eq!("1GB".parse(), Ok(Size::Fixed(1_000_000_000)));
        assert_eq!("4TB".parse(), Ok(Size::Fixed(4_000_000_000_000)));
    }

    #[test]
    fn parses_percentages() {
        assert_eq!("1%".parse(), Ok(Size::Percent(1)));
        assert_eq!("100%".parse(), Ok(Size::Percent(100)));
    }

    #[test]
    fn rejects_invalid_notations() {
        for s in [
            "",
            "1",
            "1024",
            "1kB",
            "1gb",
            "1Gb",
            "1GiB ",
            "1T",
            "1PB",
            "99999999TB",
            "1G",
            "1gib",
            "1.5GiB",
            "33.3%",
            "0%",
            "101%",
            "050%",
            "0GiB",
            "01GiB",
            "-1GiB",
            "+1GiB",
            " 1GiB",
            "1 GiB",
            "rest",
            "remaining",
            "100%FREE",
            "%",
            "GiB",
            "99999999999999TiB",
        ] {
            assert!(s.parse::<Size>().is_err(), "{s:?} should be rejected");
        }
    }

    #[test]
    fn fixed_size_rejects_percentages() {
        assert_eq!(
            FixedSize::try_from("900GiB".to_owned()),
            Ok(FixedSize(900 << 30))
        );
        assert!(FixedSize::try_from("50%".to_owned()).is_err());
    }
}

use crate::{examples, run, write};

fn validate_yaml(text: &str) -> crate::Outcome {
    run(&["validate"], &write("yaml", text))
}

/// Asserts that `text` is rejected and that each of `expected` appears in
/// the error output.
#[track_caller]
fn rejects(text: &str, expected: &[&str]) {
    let out = validate_yaml(text);
    assert!(!out.success, "accepted:\n{text}");
    for e in expected {
        assert!(out.stderr.contains(e), "missing {e:?} in:\n{}", out.stderr);
    }
}

#[track_caller]
fn accepts(text: &str) {
    let out = validate_yaml(text);
    assert!(out.success, "rejected:\n{text}\n{}", out.stderr);
}

/// A minimal valid declaration that the cases below extend.
const DISK: &str = "\
version: 1
disk:
  d0:
    match: {path: /dev/loop0}
    table: gpt
    partitions:
    - {name: p1, size: 1GiB, label: one}
    - {name: p2, size: 50%, label: ''}
    - {name: p3, size: 50%, label: ''}
";

#[test]
fn examples_are_valid() {
    for file in examples() {
        let out = run(&["validate"], &file);
        assert!(out.success, "{}:\n{}", file.display(), out.stderr);
        assert!(out.stdout.contains("valid"));
    }
}

#[test]
fn json_is_read_like_yaml() {
    for file in examples() {
        let yaml = std::fs::read_to_string(&file).unwrap();
        let value: serde_json::Value = yaml_serde::from_str(&yaml).unwrap();
        let out = run(&["validate"], &write("json", &value.to_string()));
        assert!(out.success, "{}:\n{}", file.display(), out.stderr);
    }
}

#[test]
fn file_format_is_chosen_by_extension() {
    assert!(run(&["validate"], &write("yml", DISK)).success);
    for ext in ["txt", "YAML", "conf"] {
        let out = run(&["validate"], &write(ext, DISK));
        assert!(!out.success);
        assert!(
            out.stderr.contains("unsupported file extension"),
            "{}",
            out.stderr
        );
    }
    // The content is not used to guess the format.
    let out = run(&["validate"], &write("json", DISK));
    assert!(!out.success);
}

#[test]
fn missing_file_is_reported() {
    let out = run(
        &["validate"],
        std::path::Path::new("/nonexistent/diskform.yaml"),
    );
    assert!(!out.success);
    assert!(out.stderr.contains("cannot read file"), "{}", out.stderr);
}

#[test]
fn version_must_be_the_integer_one() {
    rejects("disk: {}\n", &["missing field `version`"]);
    rejects("version: 2\n", &["unsupported version 2"]);
    rejects("version: '1'\n", &["version: invalid type: string"]);
    accepts("version: 1\n");
}

#[test]
fn values_are_not_coerced() {
    rejects(
        &DISK.replace("label: one", "label: 123"),
        &["disk.d0.partitions[0].label: invalid type: integer"],
    );
    rejects(
        &DISK.replace("label: one", "label: true"),
        &["invalid type: boolean"],
    );
    rejects(
        &DISK.replace("label: one", "label: ~"),
        &["null is not allowed"],
    );
    rejects(
        &DISK.replace("size: 1GiB", "size: 1024"),
        &["invalid type: integer"],
    );
    // A key that YAML reads as an integer is not turned into a name.
    rejects(
        "version: 1\nluks:\n  1: {device: disk.d0.p1, keyfile: /k}\n",
        &["string key"],
    );
}

#[test]
fn unknown_and_duplicate_keys_are_rejected() {
    rejects(&format!("{DISK}raid: {{}}\n"), &["unknown field `raid`"]);
    rejects(
        &DISK.replace("table: gpt", "table: gpt\n    wipe: true"),
        &["unknown field `wipe`"],
    );
    rejects(&format!("{DISK}disk: {{}}\n"), &["duplicate key `disk`"]);
    let lvm = "version: 1\nlvm:\n  vg0:\n    devices: [disk.d0.p1]\n    volumes:\n      a: {size: 1GiB}\n      a: {size: 2GiB}\n";
    rejects(lvm, &["duplicate key `a`"]);
    let json = r#"{"version": 1, "lvm": {"vg0": {"devices": [], "volumes": {"a": {"size": "1GiB"}, "a": {"size": "2GiB"}}}}}"#;
    let out = run(&["validate"], &write("json", json));
    assert!(!out.success);
    assert!(out.stderr.contains("duplicate key `a`"), "{}", out.stderr);
}

#[test]
fn only_values_seen_in_examples_are_accepted() {
    rejects(
        &DISK.replace("table: gpt", "table: msdos"),
        &["unknown variant `msdos`"],
    );
    rejects(
        &DISK.replace("size: 1GiB", "size: 1GiB, type: bios"),
        &["unknown variant `bios`"],
    );
    let fs = |format: &str| {
        format!("{DISK}filesystem:\n  f: {{device: disk.d0.p1, format: {format}, label: ''}}\n")
    };
    for format in ["vfat", "ext4", "xfs", "btrfs"] {
        accepts(&fs(format));
    }
    rejects(&fs("zfs"), &["unknown variant `zfs`"]);
}

#[test]
fn sizes_follow_the_notation() {
    for size in [
        "1GB", "1.5GiB", "33.3%", "0%", "101%", "rest", "100%FREE", "0MiB",
    ] {
        rejects(
            &DISK.replace("size: 1GiB", &format!("size: \"{size}\"")),
            &["invalid size"],
        );
    }
    rejects(
        &DISK.replace("{path: /dev/loop0}", "{path: /dev/loop0, min_size: 50%}"),
        &["a percentage is not allowed here"],
    );
}

#[test]
fn percentages_of_one_parent_must_not_exceed_100() {
    rejects(
        &DISK.replace("p3, size: 50%", "p3, size: 51%"),
        &["disk.d0.partitions: percentages add up to 101%"],
    );
    accepts(&DISK.replace("p3, size: 50%", "p3, size: 49%"));
    let lvm = |b: &str| {
        format!(
            "{DISK}lvm:\n  vg0:\n    devices: [disk.d0.p1]\n    volumes:\n      a: {{size: 60%}}\n      b: {{size: {b}}}\n"
        )
    };
    accepts(&lvm("40%"));
    rejects(
        &lvm("41%"),
        &["lvm.vg0.volumes: percentages add up to 101%"],
    );
}

#[test]
fn names_are_restricted() {
    rejects(&DISK.replace("d0:", "D0:"), &["invalid name `D0`"]);
    rejects(
        &DISK.replace("name: p1", "name: p.1"),
        &["invalid name `p.1`"],
    );
    rejects(
        &DISK.replace("name: p2", "name: p1"),
        &["duplicate partition name `p1`"],
    );
}

#[test]
fn disk_match_path_is_checked() {
    let with = |path: &str| DISK.replace("/dev/loop0", &format!("\"{path}\""));
    accepts(&with("/dev/disk/by-id/nvme-Samsung_*"));
    rejects(&with("/sys/block/sda"), &["must start with /dev/"]);
    rejects(&with("/dev/disk/*/foo"), &["only in the last component"]);
    rejects(&with("/dev/../etc/passwd"), &["`..`"]);
    rejects(&with("/dev/sd[ab"), &["unclosed `[`"]);
    rejects(
        &DISK.replace("{path: /dev/loop0}", "{min_size: 1GiB}"),
        &["missing field `path`"],
    );
}

#[test]
fn references_must_point_to_declared_block_devices() {
    let luks = |device: &str| format!("{DISK}luks:\n  c: {{device: {device}, keyfile: /run/k}}\n");
    accepts(&luks("disk.d0.p1"));
    rejects(&luks("disk.d0"), &["a disk cannot be referenced"]);
    rejects(&luks("lvm.vg0"), &["a volume group cannot be referenced"]);
    rejects(&luks("filesystem.f"), &["does not provide a block device"]);
    rejects(&luks("md.m0"), &["unknown kind `md`"]);
    rejects(
        &luks("disk.d0.p9"),
        &["luks.c.device: `disk.d0.p9` is not declared"],
    );
    rejects(&luks("disk.x.p1"), &["`disk.x.p1` is not declared"]);
    rejects(
        &format!("{DISK}luks:\n  c: {{device: disk.d0.p1, keyfile: run/k}}\n"),
        &["must be absolute"],
    );
}

#[test]
fn a_device_has_at_most_one_user() {
    let text = format!(
        "{DISK}luks:\n  c: {{device: disk.d0.p1, keyfile: /k}}\nswap:\n  s: {{device: disk.d0.p1, label: ''}}\n"
    );
    rejects(
        &text,
        &["swap.s.device: `disk.d0.p1` is already used by luks.c.device"],
    );
    let text =
        format!("{DISK}lvm:\n  vg0:\n    devices: [disk.d0.p1, disk.d0.p1]\n    volumes: {{}}\n");
    rejects(
        &text,
        &["lvm.vg0.devices[1]: `disk.d0.p1` is already used by lvm.vg0.devices[0]"],
    );
}

#[test]
fn reference_cycles_are_rejected() {
    let text = format!(
        "{DISK}luks:\n  c: {{device: lvm.vg0.lv, keyfile: /k}}\nlvm:\n  vg0:\n    devices: [luks.c]\n    volumes:\n      lv: {{size: 100%}}\n"
    );
    rejects(&text, &["reference cycle: luks.c -> lvm.vg0.lv -> luks.c"]);
}

#[test]
fn unused_devices_are_not_errors() {
    // Every partition of DISK is unused.
    accepts(DISK);
}

#[test]
fn filesystem_devices_follow_the_format() {
    let fs = |body: &str| format!("{DISK}filesystem:\n  f: {{{body}, label: ''}}\n");
    rejects(
        &fs("format: ext4"),
        &["either `device` or `devices` is required"],
    );
    rejects(
        &fs("device: disk.d0.p1, devices: [disk.d0.p2], format: btrfs"),
        &["`device` and `devices` are exclusive"],
    );
    rejects(
        &fs("devices: [disk.d0.p1], format: ext4"),
        &["`devices` is accepted only for btrfs"],
    );
    rejects(
        &fs("devices: [], format: btrfs"),
        &["filesystem.f.devices: must not be empty"],
    );
    rejects(
        &fs(
            "device: disk.d0.p1, format: xfs, btrfs: {data_profile: raid1, metadata_profile: raid1}",
        ),
        &["`btrfs` is accepted only for btrfs"],
    );
    rejects(
        &fs("device: disk.d0.p1, format: xfs, subvolumes: {a: {path: '@a'}}"),
        &["`subvolumes` is accepted only for btrfs"],
    );
    rejects(
        &fs(
            "device: disk.d0.p1, format: btrfs, btrfs: {data_profile: raid1, metadata_profile: raid1}",
        ),
        &["btrfs.data_profile: raid1 needs at least 2 devices, but 1 declared"],
    );
    rejects(
        &fs(
            "devices: [disk.d0.p1, disk.d0.p2], format: btrfs, btrfs: {data_profile: raid5, metadata_profile: raid1}",
        ),
        &["unknown variant `raid5`"],
    );
    rejects(
        &fs("device: disk.d0.p1, format: btrfs, subvolumes: {a: {path: '@a'}, b: {path: '@a'}}"),
        &["subvolume path `@a` is also used by `a`"],
    );
    rejects(
        &fs("device: disk.d0.p1, format: btrfs, subvolumes: {a: {path: /a}}"),
        &["must be relative"],
    );
}

#[test]
fn labels_are_checked() {
    // `label` is required; the empty label means no label (ADR 0008).
    rejects(
        &DISK.replace(", label: one", ""),
        &["disk.d0.partitions[0]: missing field `label`"],
    );
    accepts(&DISK.replace("label: one", "label: ''"));
    rejects(
        &DISK.replace("label: one", &format!("label: {}", "x".repeat(37))),
        &["is 37 UTF-16 code units, more than 36"],
    );
    accepts(&DISK.replace("label: one", &format!("label: {}", "x".repeat(36))));
    rejects(
        &DISK.replace(
            "{name: p2, size: 50%, label: ''}",
            "{name: p2, size: 50%, label: one}",
        ),
        &["disk.d0.partitions[1].label: label `one` is also used at disk.d0.partitions[0].label"],
    );

    let fs = |format: &str, label: &str| {
        format!(
            "{DISK}filesystem:\n  f: {{device: disk.d0.p1, format: {format}, label: {label}}}\n"
        )
    };
    for (format, limit) in [("vfat", 11), ("ext4", 16), ("xfs", 12), ("btrfs", 255)] {
        accepts(&fs(format, &"x".repeat(limit)));
        rejects(
            &fs(format, &"x".repeat(limit + 1)),
            &[&format!(
                "{format} label `{}` is {} bytes, more than {limit}",
                "x".repeat(limit + 1),
                limit + 1
            )],
        );
    }

    // Empty labels have no length limit to exceed and never collide.
    accepts(&format!(
        "{DISK}filesystem:\n  f: {{device: disk.d0.p1, format: vfat, label: ''}}\nswap:\n  s: {{device: disk.d0.p2, label: ''}}\n"
    ));
    // mkfs.fat writes `NO NAME` for no label, so it cannot be declared.
    for label in ["NO NAME", "'NO NAME  '"] {
        rejects(&fs("vfat", label), &["vfat label `NO NAME"]);
    }
    accepts(&fs("ext4", "NO NAME"));
    rejects(
        &format!("{DISK}filesystem:\n  f: {{device: disk.d0.p1, format: ext4}}\n"),
        &["filesystem.f: missing field `label`"],
    );

    let swap = |label: &str| format!("{DISK}swap:\n  s: {{device: disk.d0.p2, label: {label}}}\n");
    accepts(&swap(&"x".repeat(16)));
    rejects(&swap(&"x".repeat(17)), &["swap label"]);

    // Filesystem and swap labels share one namespace; partition labels do not.
    let text = format!(
        "{DISK}filesystem:\n  f: {{device: disk.d0.p1, format: ext4, label: one}}\nswap:\n  s: {{device: disk.d0.p2, label: one}}\n"
    );
    rejects(
        &text,
        &["swap.s.label: label `one` is also used at filesystem.f.label"],
    );
    assert!(!validate_yaml(&text).stderr.contains("partitions[0].label"));
}

#[test]
fn mount_points_are_unique() {
    let text = format!(
        "{DISK}filesystem:\n  a: {{device: disk.d0.p1, format: ext4, label: '', mount: /srv}}\n  b: {{device: disk.d0.p2, format: btrfs, label: '', subvolumes: {{s: {{path: '@s', mount: /srv}}}}}}\n"
    );
    rejects(
        &text,
        &["filesystem.b.subvolumes.s.mount: mount point `/srv` is also used at filesystem.a.mount"],
    );
    let text = format!(
        "{DISK}filesystem:\n  a: {{device: disk.d0.p1, format: ext4, label: '', mount: /srv/}}\n"
    );
    rejects(&text, &["trailing slash"]);
}

#[test]
fn device_mapper_names_are_unique() {
    let text = format!(
        "{DISK}luks:\n  vg-0-root: {{device: disk.d0.p1, keyfile: /k}}\nlvm:\n  vg-0:\n    devices: [disk.d0.p2]\n    volumes:\n      root: {{size: 1GiB}}\n  vg:\n    devices: [disk.d0.p3]\n    volumes:\n      0-root: {{size: 1GiB}}\n"
    );
    // `vg-0`/`root` maps to `vg--0-root` and `vg`/`0-root` to `vg-0--root`,
    // so neither collides with the LUKS mapping `vg-0-root`.
    accepts(&text);
    let text = format!(
        "{DISK}luks:\n  vg0-root: {{device: disk.d0.p1, keyfile: /k}}\nlvm:\n  vg0:\n    devices: [disk.d0.p2]\n    volumes:\n      root: {{size: 1GiB}}\n"
    );
    rejects(
        &text,
        &["lvm.vg0.volumes.root: device-mapper name `vg0-root` is also used by luks.vg0-root"],
    );
}

#[test]
fn all_issues_are_reported_at_once() {
    let text = format!(
        "{DISK}luks:\n  c: {{device: disk.d0.p9, keyfile: /k}}\nswap:\n  s: {{device: disk.d0.p1, label: {}}}\n",
        "x".repeat(17)
    );
    rejects(&text, &["luks.c.device", "swap.s.label"]);
}

#[test]
fn structs_are_not_read_from_sequences() {
    // Regression: serde reads a struct from a sequence by position.
    rejects(
        "- 1\n",
        &["invalid type: sequence, expected struct Declaration"],
    );
    rejects(
        &DISK.replace("{name: p1, size: 1GiB, label: one}", "[p1, 1GiB]"),
        &["disk.d0.partitions[0]: invalid type: sequence"],
    );
}

#[test]
fn empty_declarations_are_rejected() {
    // Regression: an empty document was reported as a misplaced null.
    for text in ["", "# comment only\n"] {
        rejects(text, &["the declaration is empty"]);
    }
    let out = run(&["validate"], &write("json", "null"));
    assert!(
        out.stderr.contains("the declaration is empty"),
        "{}",
        out.stderr
    );
}

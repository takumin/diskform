# diskform
Declarative block storage provisioner: partitions, RAID, LUKS, LVM and btrfs from a single spec

## Status

`validate`, `plan` and `destroy` are implemented.

`validate` reads a declaration (`.yaml`, `.yml` or `.json`) and checks it without accessing any device.

```console
$ diskform validate examples/luks-lvm-btrfs-raid1/diskform.yaml
examples/luks-lvm-btrfs-raid1/diskform.yaml: valid
```

`plan` also matches the declared disks (ADR 0003), computes absolute sizes (ADR 0002, ADR 0010, ADR 0012), and lists the operations that apply would perform.
It reads devices but never changes them; reading them usually needs root.
For each group of disks connected through the declaration, it plans to create the group if every disk is empty, leaves the group unchanged if its existing storage is configured as declared, and refuses it otherwise (ADR 0011, ADR 0015).
A group is judged configured only if every compared item can be read without changing state, so LUKS devices must be open, logical volumes active and btrfs mounted.

`destroy` erases the existing storage on the disks given with `--target disk.<name>` (repeatable) or `--all` (ADR 0004, ADR 0016).
It deactivates the volume groups and closes the LUKS mappings on those disks, erases every signature on their partitions and then the partition table and signatures on the disks themselves with `wipefs`, and checks that each disk is left empty.
Tools no longer recognize the erased storage, but the data itself is not overwritten and may be recoverable.
It refuses if anything on the disks is mounted or used as swap, if storage on them also spans disks that are not targets, or if they hold md arrays or device-mapper devices other than LUKS mappings and logical volumes.
It shows the operations and asks for `yes` before changing anything; `--auto-approve` skips only that question.

`apply`, `mount` and `unmount` (ADR 0004) are not implemented yet.
Design decisions are recorded in `docs/adr/`.

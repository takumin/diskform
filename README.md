# diskform
Declarative block storage provisioner: partitions, RAID, LUKS, LVM and btrfs from a single spec

## Status

`validate` and `plan` are implemented.

`validate` reads a declaration (`.yaml`, `.yml` or `.json`) and checks it without accessing any device.

```console
$ diskform validate examples/luks-lvm-btrfs-raid1/diskform.yaml
examples/luks-lvm-btrfs-raid1/diskform.yaml: valid
```

`plan` also matches the declared disks (ADR 0003), computes absolute sizes (ADR 0002, ADR 0010, ADR 0012), and lists the operations that apply would perform.
It reads devices but never changes them; reading them usually needs root.
For each group of disks connected through the declaration, it plans to create the group if every disk is empty, leaves the group unchanged if its existing storage is configured as declared, and refuses it otherwise (ADR 0011, ADR 0015).
A group is judged configured only if every compared item can be read without changing state, so LUKS devices must be open, logical volumes active and btrfs mounted.

`apply`, `destroy`, `mount` and `unmount` (ADR 0004) are not implemented yet.
Design decisions are recorded in `docs/adr/`.

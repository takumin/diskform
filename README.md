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
It reads devices but never changes them; reading disk signatures with blkid usually needs root.
It refuses a disk that is not empty, because judging whether existing storage already satisfies the declaration (ADR 0011) is not implemented yet.

`apply`, `destroy`, `mount` and `unmount` (ADR 0004) are not implemented yet.
Design decisions are recorded in `docs/adr/`.

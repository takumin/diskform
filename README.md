# diskform
Declarative block storage provisioner: partitions, RAID, LUKS, LVM and btrfs from a single spec

## Status

Only `validate` is implemented.
It reads a declaration (`.yaml`, `.yml` or `.json`) and checks it without accessing any device.

```console
$ diskform validate examples/luks-lvm-btrfs-raid1/diskform.yaml
examples/luks-lvm-btrfs-raid1/diskform.yaml: valid
```

`plan`, `apply`, `destroy`, `mount` and `unmount` (ADR 0004) are not implemented yet.
Design decisions are recorded in `docs/adr/`.

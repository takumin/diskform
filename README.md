# diskform
Declarative block storage provisioner: partitions, RAID, LUKS, LVM and btrfs from a single spec

## Status

`validate`, `plan`, `apply` and `destroy` are implemented.

`validate` reads a declaration (`.yaml`, `.yml` or `.json`) and checks it without accessing any device.

```console
$ diskform validate examples/luks-lvm-btrfs-raid1/diskform.yaml
examples/luks-lvm-btrfs-raid1/diskform.yaml: valid
```

`plan` also matches the declared disks (ADR 0003), computes absolute sizes (ADR 0002, ADR 0010, ADR 0012), and lists the operations that apply would perform.
It reads devices but never changes them; reading them usually needs root.
For each group of disks connected through the declaration, it plans to create the group if every disk is empty, leaves the group unchanged if its existing storage is configured as declared, and refuses it otherwise (ADR 0011, ADR 0015).
A group is judged configured only if every compared item can be read without changing state, so LUKS devices must be open, logical volumes active and btrfs mounted.

`apply` creates the storage of the groups that `plan` would create, after showing the operations and asking for `yes` (`--auto-approve` skips only that question) (ADR 0018).
It refuses if `plan` finds any problem, and does nothing for groups that are already configured.
Before each operation it checks that blkid finds nothing on the devices it writes on, so it stops instead of writing over signatures that earlier storage left inside the new partitions; erase such disks with `destroy` and apply again.
After each operation it reads back what it made, and it stops at the first failure and shows the state of the disks.
It leaves the LUKS mappings open and the volume groups active, and unmounts the btrfs that it mounts to create subvolumes, so applying again right after it refuses the group of a btrfs until the btrfs is mounted.

`destroy` erases the existing storage on the disks given with `--target disk.<name>` (repeatable) or `--all` (ADR 0004, ADR 0016).
It deactivates the volume groups and closes the LUKS mappings on those disks, erases every signature on their partitions and then the partition table and signatures on the disks themselves with `wipefs`, and checks that each disk is left empty.
Tools no longer recognize the erased storage, but the data itself is not overwritten and may be recoverable.
It refuses if anything on the disks is mounted or used as swap, if storage on them also spans disks that are not targets, or if they hold md arrays or device-mapper devices other than LUKS mappings and logical volumes.
It shows the operations and asks for `yes` before changing anything; `--auto-approve` skips only that question.

`mount` and `unmount` (ADR 0004) are not implemented yet.
Design decisions are recorded in `docs/adr/`.

## Testing

`cargo test` runs the plan-layer tests, which need no privileges and read no devices.

The apply-layer tests boot a cloud-hypervisor VM for each test and change only the blank disks of that VM (ADR 0017).
They need x86-64 Linux with read and write access to `/dev/kvm`, and `mkfs.ext4`.
Without `DISKFORM_APPLY_VM`, they are skipped and say so.

```console
$ tests/apply/fetch-vm.sh .vm
$ DISKFORM_APPLY_VM=$PWD/.vm cargo test --test apply
```

#!/bin/sh
# Downloads the VM that runs the apply-layer tests (ADR 0017) into DIR, and
# verifies every file against its pinned hash. Files already verified are
# kept, so DIR can be cached.
set -eu

dir=${1:?usage: fetch-vm.sh DIR}
mkdir -p "$dir"
cd "$dir"

hypervisor=https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/v53.0
ubuntu=https://cloud-images.ubuntu.com/releases/resolute/release-20260927

fetch() {
    name=$1 url=$2 sha256=$3
    if [ -f "$name" ] && echo "$sha256  $name" | sha256sum --check --status; then
        return
    fi
    curl --fail --location --silent --show-error --output "$name.part" "$url"
    echo "$sha256  $name.part" | sha256sum --check --quiet
    mv "$name.part" "$name"
}

fetch cloud-hypervisor "$hypervisor/cloud-hypervisor-static" \
    448af3d4e59b22c2987f7df94c213ad40fb53a10d437e42b5ee6c4fce7c29ecc
chmod +x cloud-hypervisor
fetch vmlinuz "$ubuntu/unpacked/ubuntu-26.04-server-cloudimg-amd64-vmlinuz-generic" \
    7efd88a7facf80874d781ccb2c2421f0aaaa575e3b94760430619e826f490117
fetch initrd "$ubuntu/unpacked/ubuntu-26.04-server-cloudimg-amd64-initrd-generic" \
    c9f23b4ac34a20df6a1d344b44bf2597faa48799d814cbc206b3974c6ead5215
fetch rootfs.qcow2 "$ubuntu/ubuntu-26.04-server-cloudimg-amd64.img" \
    8800651811af9a85465ad1d552add729947bb16488dddb4a9b5305a3d97332b2

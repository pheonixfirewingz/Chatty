#!/usr/bin/env bash
# Build the chatty-metal boot image: one EFI file holding the patched kernel, the
# firmware, and chatty-metal as PID 1. See ADR 0007 and docs/OPERATIONS.md.
#
#   METAL_FIRMWARE_HOST=192.168.0.97 packaging/metal/build-image.sh
#
# Firmware comes from METAL_FIRMWARE_DIR (a local /lib/firmware) or is read over SSH
# from METAL_FIRMWARE_HOST. Output: $METAL_CACHE/out/BOOTX64.EFI
#
# METAL_DEBUG=1 builds a test image with the token-protected debug agent. Its token is
# inside the image, so anyone who can fetch the image can use it. Never deploy one.
set -euo pipefail

version=${METAL_KERNEL_VERSION:-6.18.60} # Latest LTS when pinned.
sha256=${METAL_KERNEL_SHA256:-3c2f44ef6acc021a7ad444d9a2e6ec1ef488ff09368793d0004a6080f69d621e}
cpu_microcode=intel-ucode/06-9e-0c # i9-9900K: family 6, model 158, stepping 12.

ip=${METAL_IP:-192.168.0.97}
gateway=${METAL_GATEWAY:-192.168.0.1}
netmask=${METAL_NETMASK:-255.255.255.0}
broadcast=${METAL_BROADCAST:-192.168.0.255}
model=${METAL_MODEL:-/digitech/models/DeepSeek-V4.1-Flash-1017-FP8} # Inside the home subvolume.

# amdgpu's IP blocks are common 0, gmc 1, ih 2, psp 3, smu 4, dm 5, gfx 6, sdma 7, vcn 8,
# jpeg 9, mes 10. The mask turns off video encode and decode, bits 8 and 9.
cmdline="rdinit=/init panic=10 printk.devkmsg=on amdgpu.ip_block_mask=0xfffffcff"
cmdline+=" amdgpu.runpm=0 amdgpu.audio=0 ip=$ip::$gateway:$netmask::eth0:off"
cmdline+=" netconsole=6665@$ip/eth0,6666@$broadcast/ff:ff:ff:ff:ff:ff chatty.model=$model"

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
cache=${METAL_CACHE:-$HOME/.cache/chatty-metal}
kernel=$cache/kernel
source=$kernel/linux-$version
build=$kernel/build-$version
firmware=$cache/firmware
out=$cache/out

for tool in bc flex bison cc make zstd xz cpio; do
    command -v "$tool" >/dev/null || { echo "missing build tool: $tool" >&2; exit 1; }
done
mkdir -p "$kernel" "$build" "$firmware" "$out"

# Kernel source, checked against the pinned hash, patched once.
tarball=$kernel/linux-$version.tar.xz
[[ -f $tarball ]] || curl -fsSL -o "$tarball" "https://cdn.kernel.org/pub/linux/kernel/v${version%%.*}.x/linux-$version.tar.xz"
echo "$sha256  $tarball" | sha256sum -c --quiet -
patches_hash=$(cat "$here"/patches/*.patch | sha256sum | cut -d' ' -f1)
if [[ $(cat "$source/.chatty-patched" 2>/dev/null) != "$patches_hash" ]]; then
    rm -rf "$source"
    tar -xf "$tarball" -C "$kernel"
    for patch in "$here"/patches/*.patch; do
        patch -d "$source" -p1 --quiet <"$patch"
    done
    echo "$patches_hash" >"$source/.chatty-patched"
fi

# Firmware, read-only from its source and stored decompressed.
fetch_firmware() {
    local name=$1 target=$firmware/$1 ext
    [[ -s $target ]] && return
    mkdir -p "$(dirname "$target")"
    for ext in .xz .zst ""; do
        if [[ -n ${METAL_FIRMWARE_DIR:-} ]]; then
            [[ -f $METAL_FIRMWARE_DIR/$name$ext ]] && cat "$METAL_FIRMWARE_DIR/$name$ext" >"$target.tmp" || continue
        elif [[ -n ${METAL_FIRMWARE_HOST:-} ]]; then
            ssh -o BatchMode=yes "$METAL_FIRMWARE_HOST" "cat /lib/firmware/$name$ext" >"$target.tmp" 2>/dev/null || continue
        else
            echo "set METAL_FIRMWARE_DIR or METAL_FIRMWARE_HOST" >&2
            exit 1
        fi
        case $ext in
            .xz) xz -dc "$target.tmp" >"$target" ;;
            .zst) zstd -qdc "$target.tmp" >"$target" ;;
            *) mv "$target.tmp" "$target" ;;
        esac
        rm -f "$target.tmp"
        [[ -s $target ]] && return
    done
    echo "firmware not found: $name" >&2
    exit 1
}
mapfile -t firmware_files < <(grep -v '^#' "$here/firmware.list")
for name in "${firmware_files[@]}" "$cpu_microcode"; do
    fetch_firmware "$name"
done

# PID 1: fully static, tuned for the host's CPU, no build-script flag leakage.
features=()
if [[ ${METAL_DEBUG:-0} == 1 ]]; then
    token_file=$cache/debug-token
    if [[ ! -s $token_file ]]; then
        (umask 077 && head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' >"$token_file")
    fi
    export CHATTY_METAL_DEBUG_TOKEN
    CHATTY_METAL_DEBUG_TOKEN=$(<"$token_file")
    features=(--features debug-agent)
fi
(
    cd "$repo"
    RUSTFLAGS="-C target-feature=+crt-static -C target-cpu=skylake" \
        cargo build --release --locked --offline -p chatty-metal --bin chatty-metal "${features[@]}" \
        --target x86_64-unknown-linux-gnu
)
init=$(cd "$repo" && cargo metadata --offline --format-version 1 --no-deps |
    python3 -Ic 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/x86_64-unknown-linux-gnu/release/chatty-metal
[[ -x $init ]] || { echo "PID 1 binary not found at $init" >&2; exit 1; }

# Initramfs contents.
list=$out/initramfs.list
{
    # Rust's startup aborts if stdin, stdout, and stderr are closed and /dev/null is missing.
    # With no console driver they are closed, so the node has to exist before devtmpfs.
    printf 'dir /dev 0755 0 0\nnod /dev/null 0666 0 0 c 1 3\n'
    printf 'dir /proc 0555 0 0\ndir /sys 0555 0 0\ndir /model 0555 0 0\n'
    printf 'dir /lib 0755 0 0\ndir /lib/firmware 0755 0 0\ndir /lib/firmware/amdgpu 0755 0 0\n'
    printf 'file /init %s 0755 0 0\n' "$init"
    for name in "${firmware_files[@]}"; do
        printf 'file /lib/firmware/%s %s 0444 0 0\n' "$name" "$firmware/$name"
    done
} >"$list"

# Kernel configuration.
config=$out/kernel.fragment
{
    # x86 on 7.x offers only lazy preemption.
    if [[ ${version%%.*} -ge 7 ]]; then
        sed 's/^CONFIG_PREEMPT_NONE=y$/CONFIG_PREEMPT_LAZY=y/' "$here/kernel.fragment"
    else
        cat "$here/kernel.fragment"
    fi
    printf 'CONFIG_INITRAMFS_SOURCE="%s"\n' "$list"
    printf 'CONFIG_CMDLINE_BOOL=y\nCONFIG_CMDLINE="%s"\n' "$cmdline"
    printf 'CONFIG_EXTRA_FIRMWARE="%s"\nCONFIG_EXTRA_FIRMWARE_DIR="%s"\n' "$cpu_microcode" "$firmware"
    printf 'CONFIG_LOCALVERSION="-chatty-metal"\n'
} >"$config"
KCONFIG_ALLCONFIG=$config make -s -C "$source" O="$build" allnoconfig

# Fail if the kernel silently dropped or re-enabled anything.
bad=0
while IFS= read -r line; do
    case $line in
        CONFIG_*=*)
            grep -qxF -- "$line" "$build/.config" || { echo "not applied: $line" >&2; bad=1; }
            ;;
        "# CONFIG_"*" is not set")
            symbol=${line#\# }
            symbol=${symbol%% *}
            if grep -qE "^$symbol=[ym]" "$build/.config"; then
                echo "still enabled: $symbol" >&2
                bad=1
            fi
            ;;
    esac
done <"$config"
for symbol in USB USB_STORAGE EXT4_FS VFAT_FS FAT_FS ATA SCSI; do
    if grep -qE "^CONFIG_$symbol=[ym]" "$build/.config"; then
        echo "must stay out of the image: CONFIG_$symbol" >&2
        bad=1
    fi
done
((bad == 0))

make -s -j"$(nproc)" -C "$source" O="$build" bzImage
cp "$build/arch/x86/boot/bzImage" "$out/BOOTX64.EFI"
ls -l "$out/BOOTX64.EFI"
sha256sum "$out/BOOTX64.EFI"

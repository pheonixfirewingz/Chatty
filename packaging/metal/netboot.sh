#!/usr/bin/env bash
# Offer the chatty-metal image to one machine over PXE. Needs root for the DHCP and
# TFTP ports, and runs in the foreground until stopped:
#
#   sudo packaging/metal/netboot.sh
#
# It serves on every interface, including ones that appear later, so a USB network
# adapter dropping out or the laptop moving between wired and Wi-Fi does not stop it.
#
# This is proxy DHCP: the router still hands out addresses, and only the MAC below gets
# a boot offer. The offer points at $root/BOOTX64.EFI. Removing that file disarms it, so
# the firmware's PXE attempt fails and it falls through to the next boot entry.
set -euo pipefail

mac=${METAL_MAC:-04:92:26:c2:aa:63}
subnet=${METAL_SUBNET:-192.168.0.0}
user=${SUDO_USER:-$USER}
home=$(getent passwd "$user" | cut -d: -f6)
root=${METAL_TFTP_ROOT:-$home/.cache/chatty-metal/tftp}

mkdir -p "$root"
chown "$user" "$root"
echo "offering $root/BOOTX64.EFI to $mac"
exec dnsmasq --no-daemon --log-dhcp --port=0 --user="$user" \
    --except-interface=lo --bind-dynamic \
    --dhcp-range="$subnet,proxy" \
    --dhcp-mac=set:metal,"$mac" --dhcp-ignore=tag:!metal \
    --pxe-service=x86-64_EFI,chatty-metal,BOOTX64.EFI \
    --enable-tftp --tftp-root="$root" --tftp-secure

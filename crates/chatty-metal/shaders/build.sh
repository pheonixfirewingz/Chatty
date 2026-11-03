#!/bin/sh
# Rebuild the checked-in GPU code object. Needs clang and lld with the AMDGPU target.
set -eu
cd "$(dirname "$0")"
clang --target=amdgcn-amd-amdhsa -mcpu=gfx1100 -nogpulib -O3 -ffreestanding \
    -Wall -Wextra -fuse-ld=lld -o model.hsaco model.c

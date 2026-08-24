#!/usr/bin/env bash
set -Eeuo pipefail

project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
app_id=io.github.pheonixfirewingz.Chatty
runtime_branch=${CHATTY_FLATPAK_BRANCH:-25.08}
runtime=org.freedesktop.Platform
sdk=org.freedesktop.Sdk
rust_sdk=org.freedesktop.Sdk.Extension.rust-stable
arch=$(flatpak --default-arch 2>/dev/null || true)
build_root="$project_dir/build/flatpak"
source_dir="$build_root/source"
app_dir="$build_root/app"
repo_dir="$build_root/repo"
dist_dir="$project_dir/dist"

if ! command -v flatpak >/dev/null 2>&1; then
    printf '%s\n' "Flatpak is required to build the release bundle." >&2
    exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
    printf '%s\n' "Cargo is required to vendor the locked Rust dependencies." >&2
    exit 1
fi
if [[ -z "$arch" ]]; then
    printf '%s\n' "Could not determine the host Flatpak architecture." >&2
    exit 1
fi
if ! flatpak info "$runtime//$runtime_branch" >/dev/null 2>&1 ||
   ! flatpak info "$sdk//$runtime_branch" >/dev/null 2>&1 ||
   ! flatpak info "$rust_sdk//$runtime_branch" >/dev/null 2>&1; then
    printf 'Install the required runtime, SDK, and Rust SDK extension first:\n' >&2
    printf '  flatpak install flathub %s//%s %s//%s %s//%s\n' \
        "$runtime" "$runtime_branch" "$sdk" "$runtime_branch" \
        "$rust_sdk" "$runtime_branch" >&2
    exit 1
fi

version=$(cargo metadata --manifest-path "$project_dir/Cargo.toml" --no-deps --format-version 1 |
    sed -n 's/.*"name":"chatty-gui","version":"\([^"]*\)".*/\1/p')
if [[ -z "$version" ]]; then
    printf '%s\n' "Could not determine the chatty-gui version." >&2
    exit 1
fi

printf '%s\n' "Preparing locked client sources and dependencies…"
mkdir -p "$build_root" "$dist_dir"
for path in "$source_dir" "$app_dir" "$repo_dir"; do
    if [[ -e "$path" ]]; then
        find "$path" -depth -mindepth 1 -delete
    else
        mkdir -p "$path"
    fi
done

cp -p -- "$project_dir/Cargo.toml" "$project_dir/Cargo.lock" "$source_dir/"
cp -a -- "$project_dir/crates" "$source_dir/"
mkdir -p "$source_dir/packaging/flatpak" "$source_dir/.cargo"
cp -p -- "$project_dir/packaging/flatpak/$app_id.desktop" \
    "$project_dir/packaging/flatpak/$app_id.metainfo.xml" \
    "$project_dir/packaging/flatpak/$app_id.svg" \
    "$source_dir/packaging/flatpak/"
(
    cd "$source_dir"
    cargo vendor --quiet --locked vendor
    printf '%s\n' \
        '[source.crates-io]' \
        'replace-with = "vendored-sources"' \
        '' \
        '[source.vendored-sources]' \
        'directory = "vendor"' >.cargo/config.toml
)

printf '%s\n' "Building chatty-gui in the Flatpak SDK…"
flatpak build-init --arch="$arch" --sdk-extension="$rust_sdk" \
    "$app_dir" "$app_id" "$sdk" "$runtime" "$runtime_branch"
flatpak build \
    --build-dir=/run/build/chatty \
    --bind-mount="/run/build/chatty=$source_dir" \
    "$app_dir" \
    bash -Eeuo pipefail -c '
        export CARGO_HOME=/run/build/chatty/.cargo-home
        export CARGO_TARGET_DIR=/run/build/chatty/target
        export PATH=/usr/lib/sdk/rust-stable/bin:$PATH
        cargo build --release --locked --offline -p chatty-gui
        install -Dm755 target/release/chatty-gui /app/bin/chatty-gui
        install -Dm644 packaging/flatpak/io.github.pheonixfirewingz.Chatty.desktop /app/share/applications/io.github.pheonixfirewingz.Chatty.desktop
        install -Dm644 packaging/flatpak/io.github.pheonixfirewingz.Chatty.metainfo.xml /app/share/metainfo/io.github.pheonixfirewingz.Chatty.metainfo.xml
        install -Dm644 packaging/flatpak/io.github.pheonixfirewingz.Chatty.svg /app/share/icons/hicolor/scalable/apps/io.github.pheonixfirewingz.Chatty.svg
    '

flatpak build-finish \
    --command=chatty-gui \
    --share=network \
    --socket=wayland \
    --socket=fallback-x11 \
    --device=dri \
    "$app_dir"

printf '%s\n' "Exporting Flatpak bundle…"
flatpak build-export --arch="$arch" "$repo_dir" "$app_dir" "$runtime_branch"
bundle="$dist_dir/chatty-$version-$arch.flatpak"
if [[ -f "$bundle" ]]; then
    rm -f -- "$bundle"
fi
flatpak build-bundle \
    --arch="$arch" \
    --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo \
    "$repo_dir" "$bundle" "$app_id" "$runtime_branch"

printf 'Created %s\n' "$bundle"

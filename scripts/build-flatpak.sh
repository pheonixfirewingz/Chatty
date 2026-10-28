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
onnxruntime_version=1.23.2
local_remote=chatty-local
install=0
publish=0

for arg in "$@"; do
    case "$arg" in
        --install) install=1 ;;
        --publish) publish=1 ;;
        -h|--help)
            printf 'Usage: %s [--install] [--publish]\n' "$0"
            printf '  --install  Install or update the app from the local build repository.\n'
            printf '  --publish  Sign and upload the build with flatpak-repo (FLATPAK_REPO_TOOL).\n'
            exit 0
            ;;
        *)
            printf 'Unknown argument: %s\n' "$arg" >&2
            exit 1
            ;;
    esac
done

if ! command -v flatpak >/dev/null 2>&1; then
    printf '%s\n' "Flatpak is required to build the release bundle." >&2
    exit 1
fi
if ! command -v cargo >/dev/null 2>&1; then
    printf '%s\n' "Cargo is required to vendor the locked Rust dependencies." >&2
    exit 1
fi
if ! command -v curl >/dev/null 2>&1; then
    printf '%s\n' "curl is required to download ONNX Runtime before entering the Flatpak build sandbox." >&2
    exit 1
fi
publish_tool=${FLATPAK_REPO_TOOL:-flatpak-repo}
if (( publish )) && ! command -v "$publish_tool" >/dev/null 2>&1; then
    printf 'Publishing needs %s on PATH or FLATPAK_REPO_TOOL set to its path.\n' "$publish_tool" >&2
    exit 1
fi
if [[ -z "$arch" ]]; then
    printf '%s\n' "Could not determine the host Flatpak architecture." >&2
    exit 1
fi

case "$arch" in
    x86_64)
        onnxruntime_arch=x64
        onnxruntime_sha256=1fa4dcaef22f6f7d5cd81b28c2800414350c10116f5fdd46a2160082551c5f9b
        ;;
    aarch64)
        onnxruntime_arch=aarch64
        onnxruntime_sha256=7c63c73560ed76b1fac6cff8204ffe34fe180e70d6582b5332ec094810241e5c
        ;;
    *)
        printf 'Unsupported Flatpak architecture for ONNX Runtime: %s\n' "$arch" >&2
        exit 1
        ;;
esac
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
mkdir -p "$build_root" "$dist_dir" "$repo_dir"
# The repository persists between builds so installs from it can update in place.
for path in "$source_dir" "$app_dir"; do
    if [[ -e "$path" ]]; then
        find "$path" -depth -mindepth 1 -delete
    else
        mkdir -p "$path"
    fi
done

cp -p -- "$project_dir/Cargo.toml" "$project_dir/Cargo.lock" "$project_dir/LICENSE" "$source_dir/"
cp -a -- "$project_dir/crates" "$source_dir/"
mkdir -p "$source_dir/packaging/flatpak" "$source_dir/.cargo"
cp -p -- "$project_dir/packaging/flatpak/$app_id.desktop" \
    "$project_dir/packaging/flatpak/$app_id.metainfo.xml" \
    "$project_dir/packaging/flatpak/$app_id.svg" \
    "$source_dir/packaging/flatpak/"

onnxruntime_archive="$build_root/onnxruntime-linux-$onnxruntime_arch-$onnxruntime_version.tgz"
onnxruntime_url="https://github.com/microsoft/onnxruntime/releases/download/v$onnxruntime_version/$(basename "$onnxruntime_archive")"
if [[ ! -f "$onnxruntime_archive" ]] ||
   ! printf '%s  %s\n' "$onnxruntime_sha256" "$onnxruntime_archive" | sha256sum --check --status; then
    printf '%s\n' "Downloading ONNX Runtime $onnxruntime_version for $arch…"
    curl --fail --location --retry 3 --output "$onnxruntime_archive" "$onnxruntime_url"
fi
printf '%s  %s\n' "$onnxruntime_sha256" "$onnxruntime_archive" | sha256sum --check --status
mkdir -p "$source_dir/onnxruntime"
tar -xzf "$onnxruntime_archive" --strip-components=1 -C "$source_dir/onnxruntime"
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
        export ORT_LIB_LOCATION=/run/build/chatty/onnxruntime/lib
        export ORT_PREFER_DYNAMIC_LINK=1
        cargo build --release --locked --offline -p chatty-gui
        install -Dm755 target/release/chatty-gui /app/bin/chatty-gui
        mkdir -p /app/lib
        cp -a onnxruntime/lib/libonnxruntime.so* /app/lib/
        install -Dm644 packaging/flatpak/io.github.pheonixfirewingz.Chatty.desktop /app/share/applications/io.github.pheonixfirewingz.Chatty.desktop
        install -Dm644 packaging/flatpak/io.github.pheonixfirewingz.Chatty.metainfo.xml /app/share/metainfo/io.github.pheonixfirewingz.Chatty.metainfo.xml
        install -Dm644 packaging/flatpak/io.github.pheonixfirewingz.Chatty.svg /app/share/icons/hicolor/scalable/apps/io.github.pheonixfirewingz.Chatty.svg
        install -Dm644 LICENSE /app/share/licenses/io.github.pheonixfirewingz.Chatty/LICENSE
        # AppStream catalog data lets `flatpak search` and software centres list the app.
        appstreamcli compose --prefix=/ \
            --origin=io.github.pheonixfirewingz.Chatty \
            --components=io.github.pheonixfirewingz.Chatty \
            --result-root=/app \
            --data-dir=/app/share/app-info/xmls \
            --icons-dir=/app/share/app-info/icons/flatpak \
            /app
    '

flatpak build-finish \
    --command=chatty-gui \
    --share=network \
    --socket=wayland \
    --socket=pulseaudio \
    --env=HF_HOME=/var/cache/huggingface \
    --device=dri \
    "$app_dir"

printf '%s\n' "Exporting Flatpak bundle…"
flatpak build-export --arch="$arch" "$repo_dir" "$app_dir" "$runtime_branch"
flatpak build-update-repo --prune --prune-depth=1 "$repo_dir"
bundle="$dist_dir/chatty-$version-$arch.flatpak"
if [[ -f "$bundle" ]]; then
    rm -f -- "$bundle"
fi
flatpak build-bundle \
    --arch="$arch" \
    --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo \
    "$repo_dir" "$bundle" "$app_id" "$runtime_branch"

printf 'Created %s\n' "$bundle"

if (( install )); then
    ref="$app_id//$runtime_branch"
    scope=${CHATTY_FLATPAK_SCOPE:-}
    if [[ -z "$scope" ]]; then
        if flatpak info --system "$ref" >/dev/null 2>&1 &&
           ! flatpak info --user "$ref" >/dev/null 2>&1; then
            scope=system
        else
            scope=user
        fi
    fi
    if [[ "$scope" != user && "$scope" != system ]]; then
        printf 'CHATTY_FLATPAK_SCOPE must be user or system, not %s\n' "$scope" >&2
        exit 1
    fi

    # Local developer repository; commits are unsigned, so the remote skips GPG checks.
    flatpak remote-add --"$scope" --if-not-exists --no-gpg-verify \
        --title="Chatty local builds" "$local_remote" "$repo_dir"
    flatpak remote-modify --"$scope" --url="file://$repo_dir" "$local_remote"

    origin=$(flatpak info --"$scope" --show-origin "$ref" 2>/dev/null || true)
    if [[ "$origin" == "$local_remote" ]]; then
        printf 'Updating %s (%s)…\n' "$ref" "$scope"
        flatpak update --"$scope" -y "$ref"
    else
        # Switching origin (for example from a bundle install) keeps app data in ~/.var/app.
        printf 'Installing %s from %s (%s)…\n' "$ref" "$local_remote" "$scope"
        flatpak install --"$scope" -y --reinstall "$local_remote" "$ref"
    fi
fi

if (( publish )); then
    "$publish_tool" publish "$repo_dir" "app/$app_id/$arch/$runtime_branch"
fi

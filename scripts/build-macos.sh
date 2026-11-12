#!/usr/bin/env bash
# Builds a universal (Apple silicon + Intel) Chatty.app and a .dmg in dist/.
# Runs on macOS with Xcode command-line tools and rustup.
set -Eeuo pipefail

project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
build_root="$project_dir/build/macos"
dist_dir="$project_dir/dist"
onnxruntime_version=1.23.2
onnxruntime_name="onnxruntime-osx-universal2-$onnxruntime_version"
onnxruntime_sha256=49ae8e3a66ccb18d98ad3fe7f5906b6d7887df8a5edd40f49eb2b14e20885809
targets=(aarch64-apple-darwin x86_64-apple-darwin)

if [[ $(uname -s) != Darwin ]]; then
    printf '%s\n' "The macOS package must be built on macOS." >&2
    exit 1
fi
for tool in cargo rustup curl lipo codesign hdiutil install_name_tool; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf '%s is required to build the macOS package.\n' "$tool" >&2
        exit 1
    fi
done

version=$(cargo metadata --manifest-path "$project_dir/Cargo.toml" --no-deps --format-version 1 |
    sed -n 's/.*"name":"chatty-gui","version":"\([^"]*\)".*/\1/p')
if [[ -z "$version" ]]; then
    printf '%s\n' "Could not determine the chatty-gui version." >&2
    exit 1
fi

mkdir -p "$build_root" "$dist_dir"
archive="$build_root/$onnxruntime_name.tgz"
if [[ ! -f "$archive" ]] ||
   ! printf '%s  %s\n' "$onnxruntime_sha256" "$archive" | shasum -a 256 --check --status; then
    printf '%s\n' "Downloading ONNX Runtime $onnxruntime_version…"
    curl --fail --location --retry 3 --output "$archive" \
        "https://github.com/microsoft/onnxruntime/releases/download/v$onnxruntime_version/$onnxruntime_name.tgz"
fi
printf '%s  %s\n' "$onnxruntime_sha256" "$archive" | shasum -a 256 --check --status
rm -rf "$build_root/onnxruntime"
mkdir -p "$build_root/onnxruntime"
tar -xzf "$archive" --strip-components=1 -C "$build_root/onnxruntime"

export ORT_LIB_LOCATION="$build_root/onnxruntime/lib"
export ORT_PREFER_DYNAMIC_LINK=1
export MACOSX_DEPLOYMENT_TARGET=11.0
# The app bundle ships ONNX Runtime in Contents/Frameworks.
export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-rpath,@executable_path/../Frameworks"

binaries=()
for target in "${targets[@]}"; do
    rustup target add "$target" >/dev/null
    printf 'Building chatty-gui for %s…\n' "$target"
    cargo build --manifest-path "$project_dir/Cargo.toml" --release --locked \
        -p chatty-gui --target "$target"
    binaries+=("$project_dir/target/$target/release/chatty-gui")
done

app="$build_root/Chatty.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/chatty-gui" "${binaries[@]}"
dylib=$(basename "$(readlink "$ORT_LIB_LOCATION/libonnxruntime.dylib" || echo libonnxruntime.$onnxruntime_version.dylib)")
cp "$ORT_LIB_LOCATION/$dylib" "$app/Contents/Frameworks/"
# Point the executable at the bundled copy whatever install name it recorded.
recorded=$(otool -L "$app/Contents/MacOS/chatty-gui" | awk '/libonnxruntime/ {print $1; exit}')
if [[ -n "$recorded" && "$recorded" != "@rpath/$dylib" ]]; then
    install_name_tool -change "$recorded" "@rpath/$dylib" "$app/Contents/MacOS/chatty-gui"
fi
sed "s/@VERSION@/$version/g" "$project_dir/packaging/macos/Info.plist" >"$app/Contents/Info.plist"
cp "$project_dir/packaging/macos/chatty.icns" "$app/Contents/Resources/chatty.icns"
cp "$project_dir/LICENSE" "$app/Contents/Resources/LICENSE"

# Sign with CHATTY_MACOS_SIGN_IDENTITY when set; otherwise sign ad hoc, which
# Apple silicon requires to run at all. Ad hoc builds are not notarized.
# The hardened runtime, which notarization needs, only loads libraries signed
# by the same team, so it is enabled only for a real identity.
identity=${CHATTY_MACOS_SIGN_IDENTITY:--}
sign_flags=(--force --sign "$identity")
if [[ "$identity" != - ]]; then
    sign_flags+=(--options runtime --timestamp)
fi
codesign "${sign_flags[@]}" "$app/Contents/Frameworks/$dylib"
codesign "${sign_flags[@]}" "$app"
codesign --verify --strict "$app"

dmg="$dist_dir/chatty-$version-macos-universal.dmg"
staging="$build_root/dmg"
rm -rf "$staging" "$dmg"
mkdir -p "$staging"
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
hdiutil create -volname "Chatty $version" -srcfolder "$staging" -fs HFS+ -format UDZO "$dmg" >/dev/null
printf 'Created %s\n' "$dmg"

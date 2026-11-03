#!/usr/bin/env bash
# Download the chatty-metal model files into a chosen directory. Run this from the
# host's normal OS; the appliance itself only ever reads the result.
#
#   scripts/fetch-metal-model.sh /path/to/model-dir
#
# Safe to rerun: finished files are skipped and partial files resume. A file only
# gets its final name after its size and SHA-256 match the pinned commit.
set -euo pipefail

repo=dealignai/DeepSeek-V4.1-Flash-1017-UNCENSORED-FP8
dest=${1:?usage: fetch-metal-model.sh DIR}
api=https://huggingface.co/api/models/$repo
mkdir -p "$dest/inference"

sha=$(curl -fsS --retry 5 --retry-all-errors "$api" | python3 -Ic 'import json,sys; print(json.load(sys.stdin)["sha"])')
manifest=$(mktemp)
trap 'rm -f "$manifest"' EXIT
curl -fsS --retry 5 --retry-all-errors "$api/tree/$sha?recursive=true" | python3 -Ic '
import json, re, sys
keep = re.compile(r"model-\d+-of-\d+\.safetensors|model\.safetensors\.index\.json|config\.json|tokenizer(_config)?\.json|inference/config\.json")
for entry in json.load(sys.stdin):
    if entry["type"] == "file" and keep.fullmatch(entry["path"]):
        print(entry["path"], entry["size"], entry.get("lfs", {}).get("oid", "-"))
' >"$manifest"

shards=$(grep -c '^model-' "$manifest" || true)
[[ $shards == 48 ]] || { echo "expected 48 shards, found $shards" >&2; exit 1; }

needed=0
while read -r path size _; do
    [[ -f $dest/$path ]] || needed=$((needed + size))
done <"$manifest"
free=$(df --output=avail -B1 "$dest" | tail -1)
((free > needed)) || { echo "need $needed bytes, have $free" >&2; exit 1; }
echo "commit $sha: $needed bytes to fetch, $free free"

while read -r path size oid; do
    target=$dest/$path
    [[ -f $target ]] && continue
    part=$target.part
    if [[ ! -f $part || $(stat -c %s "$part") -lt $size ]]; then
        curl -fsSL --retry 50 --retry-all-errors --retry-delay 10 -C - -o "$part" \
            "https://huggingface.co/$repo/resolve/$sha/$path"
    fi
    actual=$(stat -c %s "$part")
    [[ $actual == "$size" ]] || { rm -f "$part"; echo "$path: got $actual bytes, expected $size" >&2; exit 1; }
    if [[ $oid != - ]]; then
        hash=$(sha256sum "$part" | cut -d' ' -f1)
        [[ $hash == "$oid" ]] || { rm -f "$part"; echo "$path: SHA-256 mismatch" >&2; exit 1; }
    fi
    mv "$part" "$target"
    echo "done $path"
done <"$manifest"
echo "complete: $dest"

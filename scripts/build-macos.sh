#!/usr/bin/env bash
set -euo pipefail
test "$(uname -s)" = Darwin
test "$(uname -m)" = arm64
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_root/src-tauri/target}"
export MACOSX_DEPLOYMENT_TARGET=13.0

# Share the official archive and hash pins with the Windows build.
python3 - "$repo_root" <<'PY'
import hashlib, io, pathlib, re, sys, urllib.request, zipfile
root = pathlib.Path(sys.argv[1])
specs = re.findall(r"Weight = (\d+); Name = '([^']+)'; Hash = '([^']+)'", (root/'scripts/prepare-fonts.ps1').read_text())
assert len(specs) == 4, 'Cannot read the four pinned font definitions'
directory = root/'src-tauri/assets/fonts'
valid = lambda path, expected: path.exists() and hashlib.sha256(path.read_bytes()).hexdigest().upper() == expected
if not all(valid(directory/f'Satoshi-{weight}.otf', digest) for weight, name, digest in specs):
    with urllib.request.urlopen('https://api.fontshare.com/v2/fonts/download/satoshi', timeout=120) as response:
        archive = zipfile.ZipFile(io.BytesIO(response.read()))
    directory.mkdir(parents=True, exist_ok=True)
    for weight, name, digest in specs:
        data = archive.read(f'Satoshi_Complete/Fonts/OTF/Satoshi-{name}.otf')
        assert hashlib.sha256(data).hexdigest().upper() == digest, f'Fontshare changed Satoshi {name}'
        (directory/f'Satoshi-{weight}.otf').write_bytes(data)
print('Satoshi fonts verified.')
PY

cargo build --locked --release --package annotate --manifest-path "$repo_root/src-tauri/Cargo.toml" --target aarch64-apple-darwin
cargo build --locked --release --package whisper-worker --no-default-features --features metal --manifest-path "$repo_root/src-tauri/Cargo.toml" --target aarch64-apple-darwin
release_dir="$CARGO_TARGET_DIR/aarch64-apple-darwin/release"
cp "$release_dir/whisper-worker" "$release_dir/whisper-worker-metal"
printf '{"cmd":"backend"}\n{"cmd":"quit"}\n' | "$release_dir/whisper-worker-metal" | python3 -c 'import json,sys; assert json.loads(sys.stdin.readline()) == {"ok":True,"backend":"metal"}'
printf 'Apple silicon executable: %s\n' "$release_dir/annotate"

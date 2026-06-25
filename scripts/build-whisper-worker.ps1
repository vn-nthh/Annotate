$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repoRoot 'src-tauri\crates\whisper-worker\Cargo.toml'
$source = Join-Path $repoRoot 'src-tauri\target\release\whisper-worker.exe'
$destinationDir = Join-Path $repoRoot 'src-tauri\binaries'
$destination = Join-Path $destinationDir 'whisper-worker-x86_64-pc-windows-msvc.exe'

& cargo build --release --locked --manifest-path $manifest
if ($LASTEXITCODE -ne 0) {
  throw "whisper-worker build failed with exit code $LASTEXITCODE"
}

New-Item -ItemType Directory -Force -Path $destinationDir | Out-Null
Copy-Item -LiteralPath $source -Destination $destination -Force
Write-Host "Built sidecar: $destination"

param(
  [ValidateSet('Debug', 'Release')][string]$Configuration = 'Release',
  [switch]$ReuseWorker,
  [switch]$Run
)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repoRoot 'src-tauri\Cargo.toml'
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path ([IO.Path]::GetTempPath()) 'annotate-rust-target' }
$env:CARGO_TARGET_DIR = [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)

& (Join-Path $PSScriptRoot 'prepare-fonts.ps1')
& (Join-Path $PSScriptRoot 'build-whisper-worker.ps1') -ReuseExisting:$ReuseWorker

$cargoArgs = @('build', '--package', 'annotate', '--locked', '--manifest-path', $manifest)
if ($Configuration -eq 'Release') { $cargoArgs += '--release' }
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { throw "Native build failed with exit code $LASTEXITCODE" }

$profile = $Configuration.ToLowerInvariant()
$outputDir = Join-Path $env:CARGO_TARGET_DIR $profile
$worker = Join-Path $repoRoot 'src-tauri\binaries\whisper-worker-vulkan-x86_64-pc-windows-msvc.exe'
Copy-Item -LiteralPath $worker -Destination (Join-Path $outputDir 'whisper-worker-vulkan.exe') -Force
$executable = Join-Path $outputDir 'annotate.exe'
Write-Host "Native executable: $executable"
if ($Run) { & $executable }

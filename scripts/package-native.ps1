param([switch]$SkipBuild, [switch]$ReuseWorker)
$ErrorActionPreference = 'Stop'
$repoRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
if (-not $SkipBuild) { & (Join-Path $PSScriptRoot 'build-native.ps1') -ReuseWorker:$ReuseWorker }
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path ([IO.Path]::GetTempPath()) 'annotate-rust-target' }
$env:CARGO_TARGET_DIR = [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)
$releaseDir = Join-Path $env:CARGO_TARGET_DIR 'release'
$manifest = Get-Content -Raw -LiteralPath (Join-Path $repoRoot 'src-tauri\Cargo.toml')
$version = [regex]::Match($manifest, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $version) { throw 'Cannot find the Annotate package version.' }

$dist = Join-Path $repoRoot 'dist'
$packageDir = [IO.Path]::GetFullPath((Join-Path $dist "Annotate-$version-windows-x64"))
# Rebuild only this script's own output directory, never a caller-provided path.
if (-not $packageDir.StartsWith($dist + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
  throw "Invalid package path: $packageDir"
}
if (Test-Path -LiteralPath $packageDir) { Remove-Item -LiteralPath $packageDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $packageDir | Out-Null
foreach ($name in @('annotate.exe', 'DirectML.dll')) {
  $source = Join-Path $releaseDir $name
  if (-not (Test-Path -LiteralPath $source)) { throw "Required native build output missing: $source" }
  Copy-Item -LiteralPath $source -Destination $packageDir
}
$worker = Join-Path $repoRoot 'src-tauri\binaries\whisper-worker-vulkan-x86_64-pc-windows-msvc.exe'
if (-not (Test-Path -LiteralPath $worker)) { throw "Vulkan worker missing: $worker" }
Copy-Item -LiteralPath $worker -Destination (Join-Path $packageDir 'whisper-worker-vulkan.exe')
# Support a custom dynamically linked CPU ONNX Runtime build without copying GPU providers.
foreach ($name in @('onnxruntime.dll', 'onnxruntime_providers_shared.dll')) {
  $source = Join-Path $releaseDir $name
  if (Test-Path -LiteralPath $source) { Copy-Item -LiteralPath $source -Destination $packageDir }
}
Copy-Item -LiteralPath (Join-Path $repoRoot 'README.md') -Destination $packageDir
Copy-Item -LiteralPath (Join-Path $repoRoot 'src-tauri\assets\fonts\FFL.txt') -Destination (Join-Path $packageDir 'Satoshi-FFL.txt')
[IO.File]::WriteAllText((Join-Path $packageDir 'Read me.txt'), @"
Annotate $version for Windows 10/11 x64

Extract this whole folder and launch annotate.exe. Keep the executable, Vulkan
worker and DirectML.dll together. The Microsoft Visual C++ 2015-2022 x64 Runtime
is required: https://aka.ms/vc14/vc_redist.x64.exe

Local Whisper uses Vulkan. Install a current GPU driver from your GPU vendor;
the Vulkan SDK is only needed to build the worker, not to use the application.
Models are downloaded from Settings when requested. No WebView2 is required.

Settings and data remain in your Windows profile when this folder is replaced.
"@, (New-Object Text.UTF8Encoding($false)))
$archivePath = Join-Path $dist "Annotate-$version-windows-x64.zip"
Compress-Archive -LiteralPath $packageDir -DestinationPath $archivePath -Force -ErrorAction Stop
Write-Host "Native package: $archivePath"

# The installer upgrades previous Tauri NSIS installs in place.
$makensis = (Get-Command makensis -ErrorAction SilentlyContinue).Source
if (-not $makensis) {
  # Standard NSIS install, or the copy cached by earlier Tauri builds.
  $makensis = @(
    (Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe'),
    (Join-Path $env:LOCALAPPDATA 'tauri\NSIS\makensis.exe')
  ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
  if (-not $makensis) { throw 'NSIS (makensis) is required to build the installer.' }
}
$installerPath = Join-Path $dist "Annotate-$version-windows-x64-setup.exe"
& $makensis /V2 "/DVERSION=$version" "/DSOURCE_DIR=$packageDir" "/DICON=$(Join-Path $repoRoot 'src-tauri\icons\icon.ico')" "/DOUTFILE=$installerPath" (Join-Path $PSScriptRoot 'installer.nsi')
if ($LASTEXITCODE -ne 0) { throw 'Installer build failed.' }
Write-Host "Native installer: $installerPath"

param([switch]$ReuseExisting)
$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repoRoot 'src-tauri\crates\whisper-worker\Cargo.toml'
$destinationDir = Join-Path $repoRoot 'src-tauri\binaries'
$destination = Join-Path $destinationDir 'whisper-worker-vulkan-x86_64-pc-windows-msvc.exe'

# Vulkan's nested shader-generator build exceeds MSVC's path limits in many
# profile/workspace folders. Keep its cache independent of the native app.
$workerTargetDir = $env:WHISPER_WORKER_TARGET_DIR
if (-not $workerTargetDir) {
  $workerTargetDir = Join-Path ([IO.Path]::GetPathRoot($repoRoot)) 'aw-target'
}
$workerTargetDir = [IO.Path]::GetFullPath($workerTargetDir)

function Test-VulkanAvailable {
  if ($env:VULKAN_SDK -and (Test-Path $env:VULKAN_SDK)) {
    return $true
  }
  # Fall back to common install locations if the env var is not set in this shell.
  $candidates = @(
    'C:\VulkanSDK',
    (Join-Path $env:ProgramFiles 'VulkanSDK')
  )
  foreach ($root in $candidates) {
    if (-not (Test-Path $root)) { continue }
    $latest = Get-ChildItem -Path $root -Directory -ErrorAction SilentlyContinue |
      Sort-Object Name -Descending |
      Select-Object -First 1
    if ($latest) {
      $env:VULKAN_SDK = $latest.FullName
      $env:Path = "$($latest.FullName)\Bin;$env:Path"
      Write-Host "Detected Vulkan SDK at $env:VULKAN_SDK"
      return $true
    }
  }
  return $false
}

if (-not ($ReuseExisting -and (Test-Path -LiteralPath $destination))) {
  if (Test-VulkanAvailable) {
    Write-Host "Building the Vulkan Whisper worker in $workerTargetDir..."
    $appTargetDir = $env:CARGO_TARGET_DIR
    try {
      $env:CARGO_TARGET_DIR = $workerTargetDir
      & cargo build --release --locked --manifest-path $manifest --features vulkan
      if ($LASTEXITCODE -ne 0) { throw "Vulkan worker build failed with exit code $LASTEXITCODE. Use a short WHISPER_WORKER_TARGET_DIR for MSVC." }
    } finally { $env:CARGO_TARGET_DIR = $appTargetDir }
    $source = Join-Path $workerTargetDir 'release\whisper-worker.exe'
    if (-not (Test-Path -LiteralPath $source)) { throw "Worker build output missing: $source" }
    New-Item -ItemType Directory -Force -Path $destinationDir | Out-Null
    Copy-Item -LiteralPath $source -Destination $destination -Force
  } elseif (-not (Test-Path -LiteralPath $destination)) {
    throw 'Install the Vulkan SDK, set VULKAN_SDK, and run scripts/build-whisper-worker.ps1.'
  } else {
    Write-Host "Vulkan SDK unavailable; using existing worker: $destination"
  }
}

$startInfo = New-Object Diagnostics.ProcessStartInfo
$startInfo.FileName = $destination
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.RedirectStandardInput = $true
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true
# Windows PowerShell's redirected stdin uses the console input encoding, which
# can add a UTF-8 BOM that makes the worker reject the first JSON command.
$inputEncoding = [Console]::InputEncoding
[Console]::InputEncoding = New-Object Text.UTF8Encoding $false
try { $process = [Diagnostics.Process]::Start($startInfo) } finally { [Console]::InputEncoding = $inputEncoding }
try {
  $process.StandardInput.WriteLine('{"cmd":"backend"}')
  $process.StandardInput.WriteLine('{"cmd":"quit"}')
  $process.StandardInput.Close()
  if (-not $process.WaitForExit(15000)) { $process.Kill(); throw 'Vulkan worker protocol check timed out.' }
  $response = $process.StandardOutput.ReadLine() | ConvertFrom-Json
  if ($process.ExitCode -ne 0 -or -not $response.ok -or $response.backend -ne 'vulkan') {
    throw "The worker is not a working Vulkan build: $($process.StandardError.ReadToEnd())"
  }
} finally { $process.Dispose() }
Write-Host "Vulkan worker verified: $destination"

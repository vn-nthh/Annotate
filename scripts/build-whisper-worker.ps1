$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repoRoot 'src-tauri\crates\whisper-worker\Cargo.toml'
$destinationDir = Join-Path $repoRoot 'src-tauri\binaries'

# MSVC file-tracker (FTK1011) often fails when OUT_DIR is under a long/spaced
# path such as "F:\Standalone Annotate\...". Prefer a short target dir for
# native whisper.cpp builds unless the caller already set CARGO_TARGET_DIR.
if (-not $env:CARGO_TARGET_DIR) {
  $env:CARGO_TARGET_DIR = 'F:\w-target'
  Write-Host "Using short CARGO_TARGET_DIR=$env:CARGO_TARGET_DIR (avoids MSVC path-length issues)"
}
$targetDir = Join-Path $env:CARGO_TARGET_DIR 'release'
New-Item -ItemType Directory -Force -Path $env:CARGO_TARGET_DIR | Out-Null

function Test-CudaAvailable {
  if ($env:CUDA_PATH -and (Test-Path $env:CUDA_PATH)) {
    return $true
  }
  return $false
}

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

function Build-Worker {
  param(
    [Parameter(Mandatory = $true)][string]$Feature,
    [Parameter(Mandatory = $true)][string]$SidecarStem
  )

  Write-Host "Building whisper-worker with feature '$Feature'..."
  & cargo build --release --manifest-path $manifest --no-default-features --features $Feature
  if ($LASTEXITCODE -ne 0) {
    throw "whisper-worker ($Feature) build failed with exit code $LASTEXITCODE"
  }

  $source = Join-Path $targetDir 'whisper-worker.exe'
  if (-not (Test-Path $source)) {
    throw "Expected build output missing: $source"
  }

  $destination = Join-Path $destinationDir "$SidecarStem-x86_64-pc-windows-msvc.exe"
  New-Item -ItemType Directory -Force -Path $destinationDir | Out-Null
  Copy-Item -LiteralPath $source -Destination $destination -Force
  Write-Host "Built sidecar: $destination"
}

$builtAny = $false
$cudaOk = Test-CudaAvailable
$vulkanOk = Test-VulkanAvailable

if ($cudaOk) {
  Build-Worker -Feature 'cuda' -SidecarStem 'whisper-worker'
  $builtAny = $true
} else {
  $existingCuda = Join-Path $destinationDir 'whisper-worker-x86_64-pc-windows-msvc.exe'
  if (Test-Path $existingCuda) {
    Write-Host "CUDA toolkit not detected; keeping existing CUDA sidecar: $existingCuda"
    $builtAny = $true
  } else {
    Write-Warning "CUDA toolkit not detected (CUDA_PATH unset) and no prebuilt CUDA sidecar found."
  }
}

if ($vulkanOk) {
  Build-Worker -Feature 'vulkan' -SidecarStem 'whisper-worker-vulkan'
  $builtAny = $true
} else {
  $existingVulkan = Join-Path $destinationDir 'whisper-worker-vulkan-x86_64-pc-windows-msvc.exe'
  if (Test-Path $existingVulkan) {
    Write-Host "Vulkan SDK not detected; keeping existing Vulkan sidecar: $existingVulkan"
    $builtAny = $true
  } else {
    Write-Warning "Vulkan SDK not detected (VULKAN_SDK unset) and no prebuilt Vulkan sidecar found."
    Write-Warning "Install the LunarG Vulkan SDK, then re-run: npm run build:worker"
  }
}

if (-not $builtAny) {
  throw "No whisper-worker sidecars were built or available. Install CUDA toolkit and/or Vulkan SDK."
}

# Tauri externalBin requires every listed sidecar to exist at package time.
$required = @(
  'whisper-worker-x86_64-pc-windows-msvc.exe',
  'whisper-worker-vulkan-x86_64-pc-windows-msvc.exe'
)
foreach ($name in $required) {
  $path = Join-Path $destinationDir $name
  if (-not (Test-Path $path)) {
    throw "Required sidecar missing for Tauri bundling: $path"
  }
}

Write-Host "Whisper worker sidecars ready in $destinationDir"
Get-ChildItem $destinationDir -Filter 'whisper-worker*.exe' | ForEach-Object {
  Write-Host ("  {0}  ({1:N1} MB)" -f $_.Name, ($_.Length / 1MB))
}

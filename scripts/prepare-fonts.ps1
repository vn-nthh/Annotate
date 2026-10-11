param([switch]$Force)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

$repoRoot = Split-Path -Parent $PSScriptRoot
$fontDir = Join-Path $repoRoot 'src-tauri\assets\fonts'
# Unmodified originals from Fontshare. Do not check the font files into Git.
$fonts = @(
  @{ Weight = 300; Name = 'Light'; Hash = 'F9B532A143181BC76ADF9FD5968708C502C807ACF7B7B1F0A0D46ABA293DD2BA' },
  @{ Weight = 400; Name = 'Regular'; Hash = '711C6243CDC5431F9CC966E4DE18BFB940365BAD81ACFFD1E7948DBE3F254386' },
  @{ Weight = 500; Name = 'Medium'; Hash = '93330866D109F6B2E298748958EC6FA4010CACEF586783F281A0B268CAB7FC6E' },
  @{ Weight = 700; Name = 'Bold'; Hash = '50E4F9B7C1864C50761D729D6001BFAC708C80457FA6FC41559A8AB1BD2573FF' }
)
if (-not $Force) {
  $missing = @($fonts | Where-Object {
    $fontPath = Join-Path $fontDir "Satoshi-$($_.Weight).otf"
    -not (Test-Path -LiteralPath $fontPath) -or (Get-FileHash -LiteralPath $fontPath -Algorithm SHA256).Hash -ne $_.Hash
  })
  if ($missing.Count -eq 0) { Write-Host 'Satoshi fonts verified.'; return }
}

$archivePath = [IO.Path]::GetTempFileName()
$archive = $null
try {
  Write-Host 'Downloading Satoshi originals from Fontshare...'
  Invoke-WebRequest -Uri 'https://api.fontshare.com/v2/fonts/download/satoshi' -OutFile $archivePath -UseBasicParsing
  $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
  New-Item -ItemType Directory -Force -Path $fontDir | Out-Null
  foreach ($font in $fonts) {
    $entry = $archive.GetEntry("Satoshi_Complete/Fonts/OTF/Satoshi-$($font.Name).otf")
    if (-not $entry) { throw "Satoshi-$($font.Name).otf is missing from the official archive." }
    $bytes = New-Object IO.MemoryStream
    $stream = $entry.Open()
    try { $stream.CopyTo($bytes) } finally { $stream.Dispose() }
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { $hash = [BitConverter]::ToString($hasher.ComputeHash($bytes.ToArray())).Replace('-', '') }
    finally { $hasher.Dispose() }
    if ($hash -ne $font.Hash) { throw "Fontshare changed Satoshi-$($font.Name).otf. Review its license and layout before updating the pinned hash." }
    [IO.File]::WriteAllBytes((Join-Path $fontDir "Satoshi-$($font.Weight).otf"), $bytes.ToArray())
    $bytes.Dispose()
  }
} finally {
  if ($archive) { $archive.Dispose() }
  Remove-Item -LiteralPath $archivePath -Force
}
Write-Host 'Satoshi fonts verified and ready for application embedding.'

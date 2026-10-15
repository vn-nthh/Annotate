param([Parameter(Mandatory = $true)][string]$Archive)
$ErrorActionPreference = 'Stop'
$archivePath = (Resolve-Path -LiteralPath $Archive).Path
$temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$testDir = Join-Path $temporaryRoot ('annotate-package-smoke-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testDir | Out-Null
Expand-Archive -LiteralPath $archivePath -DestinationPath $testDir
$packageDir = (Get-ChildItem -LiteralPath $testDir -Directory | Select-Object -First 1).FullName
if (-not $packageDir) { throw "Package contains no app folder: $archivePath" }
foreach ($file in @('annotate.exe', 'whisper-worker-vulkan.exe', 'DirectML.dll')) {
  $path = Join-Path $packageDir $file
  if (-not (Test-Path -LiteralPath $path) -or (Get-Item -LiteralPath $path).Length -eq 0) {
    throw "Package is missing a complete $file"
  }
}
$browserFiles = @(Get-ChildItem -LiteralPath $packageDir -Recurse -File | Where-Object {
  $_.Extension -in @('.html', '.js', '.ttf', '.otf') -or $_.Name -match '(?i)(cuda|tauri|webview)'
})
if ($browserFiles.Count) { throw 'The native package contains browser assets, raw fonts, or a removed backend.' }

# Exercise the extracted worker independently of the checkout and build cache.
$workerInfo = New-Object Diagnostics.ProcessStartInfo
$workerInfo.FileName = Join-Path $packageDir 'whisper-worker-vulkan.exe'
$workerInfo.WorkingDirectory = $packageDir
$workerInfo.UseShellExecute = $false
$workerInfo.CreateNoWindow = $true
$workerInfo.RedirectStandardInput = $true
$workerInfo.RedirectStandardOutput = $true
$workerInfo.RedirectStandardError = $true
$worker = [Diagnostics.Process]::Start($workerInfo)
try {
  $workerOutput = $worker.StandardOutput.ReadToEndAsync()
  $workerErrors = $worker.StandardError.ReadToEndAsync()
  $worker.StandardInput.WriteLine('{"cmd":"backend"}')
  $worker.StandardInput.WriteLine('{"cmd":"quit"}')
  $worker.StandardInput.Close()
  if (-not $worker.WaitForExit(15000)) { $worker.Kill(); throw 'Packaged Vulkan worker timed out.' }
  $response = ($workerOutput.GetAwaiter().GetResult() -split '\r?\n')[0] | ConvertFrom-Json
  if ($worker.ExitCode -ne 0 -or -not $response.ok -or $response.backend -ne 'vulkan') {
    throw "Packaged Vulkan worker failed: $($workerErrors.GetAwaiter().GetResult())"
  }
} finally { $worker.Dispose() }

$captureDir = Join-Path $testDir 'capture'
$startInfo = New-Object Diagnostics.ProcessStartInfo
$startInfo.FileName = Join-Path $packageDir 'annotate.exe'
$startInfo.Arguments = '--capture-ui "' + $captureDir + '"'
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.WorkingDirectory = $packageDir
$startInfo.RedirectStandardError = $true
$process = [Diagnostics.Process]::Start($startInfo)
try {
  $appErrors = $process.StandardError.ReadToEndAsync()
  if (-not $process.WaitForExit(90000)) { $process.Kill(); throw "Packaged UI capture timed out; files remain at $testDir" }
  if ($process.ExitCode -ne 0) { throw "Packaged UI capture failed: $($appErrors.GetAwaiter().GetResult())" }
} finally { $process.Dispose() }
$sceneCount = @(Get-ChildItem -LiteralPath $captureDir -Filter '*.png').Count
$scaledCount = @(Get-ChildItem -LiteralPath (Join-Path $captureDir '150') -Filter '*.png' -ErrorAction SilentlyContinue).Count
$checksPath = Join-Path $captureDir 'checks.json'
if (-not (Test-Path -LiteralPath $checksPath) -or $sceneCount -lt 80 -or
    $scaledCount -lt 8) {
  throw "Packaged app did not render its native test screens; files remain at $testDir"
}
$checksJson = Get-Content -LiteralPath $checksPath -Raw -Encoding UTF8
$checks = ConvertFrom-Json -InputObject $checksJson
if (-not $checksJson.TrimStart().StartsWith('[') -or $checks.Count -lt 33 -or
    @($checks | Where-Object { $_ -isnot [string] -or [string]::IsNullOrWhiteSpace($_) }).Count -ne 0) {
  throw "Packaged UI harness produced no valid passed-check array; files remain at $testDir"
}
Write-Host "Native app rendered $sceneCount scenes plus $scaledCount at 150% and passed $($checks.Count) checks; its Vulkan worker responds in isolation."
# The test owns this unique temporary directory.
if ([IO.Path]::GetFullPath((Split-Path -Parent $testDir)) -ne $temporaryRoot.TrimEnd('\') -or
    (Split-Path -Leaf $testDir) -notlike 'annotate-package-smoke-*') { throw 'Invalid smoke-test cleanup path.' }
Remove-Item -LiteralPath $testDir -Recurse -Force

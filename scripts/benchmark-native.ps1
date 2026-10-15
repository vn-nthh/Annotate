param(
  [Parameter(Mandatory = $true)][string]$NativeExecutable,
  [string]$BaselineExecutable,
  [ValidateRange(1, 10)][int]$Runs = 3,
  [string]$OutputDirectory = (Join-Path ([IO.Path]::GetTempPath()) ('annotate-benchmark-' + [Guid]::NewGuid().ToString('N')))
)
$ErrorActionPreference = 'Stop'
$nativePath = (Resolve-Path -LiteralPath $NativeExecutable).Path
$nativeBytes = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($nativePath))
if (-not $nativeBytes.Contains('ANNOTATE_UI_TEST') -or -not $nativeBytes.Contains('--benchmark')) {
  throw 'Use the new native executable with its isolated --benchmark mode.'
}
$nativeBytes = $null
$outputPath = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $outputPath) { throw "Use a new output directory: $outputPath" }
$applications = @([pscustomobject]@{ Name = 'native'; Path = $nativePath })
if ($BaselineExecutable) {
  $baselinePath = (Resolve-Path -LiteralPath $BaselineExecutable).Path
  $isolationPath = Join-Path (Split-Path -Parent $baselinePath) 'baseline-isolation.json'
  $isolation = Get-Content -LiteralPath $isolationPath -Raw | ConvertFrom-Json
  if ($isolation.identifier -ne 'com.annotate.benchmark' -or
      $isolation.credentialHandlers -ne 'absent/no-op' -or
      $isolation.executableSha256 -ne (Get-FileHash -LiteralPath $baselinePath -Algorithm SHA256).Hash) {
    throw 'The baseline must be rebuilt with isolated credentials and a matching executable hash.'
  }
  $applications += [pscustomobject]@{ Name = 'tauri'; Path = $baselinePath }
}
New-Item -ItemType Directory -Path $outputPath | Out-Null

function Wait-Report([string]$Path, [Diagnostics.Process]$Process) {
  $deadline = [DateTime]::UtcNow.AddSeconds(30)
  while ([DateTime]::UtcNow -lt $deadline) {
    if (Test-Path -LiteralPath $Path) {
      try { return (Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json) } catch {}
    }
    if ($Process.HasExited) { throw "App exited before writing $Path (code $($Process.ExitCode))." }
    Start-Sleep -Milliseconds 20
  }
  throw "App did not write $Path within 30 seconds."
}

function Read-ProcessTree([int]$RootId, [hashtable]$Owned) {
  # Read-only OS counters; include WebView browser, renderer, GPU and utility children.
  $snapshot = @(Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId)
  $ids = New-Object 'Collections.Generic.HashSet[int]'
  [void]$ids.Add($RootId)
  do {
    $added = $false
    foreach ($item in $snapshot) {
      if ($ids.Contains([int]$item.ParentProcessId) -and $ids.Add([int]$item.ProcessId)) { $added = $true }
    }
  } while ($added)
  $details = @()
  foreach ($processId in $ids) {
    $item = $null
    try {
      $item = [Diagnostics.Process]::GetProcessById($processId)
      $item.Refresh()
      $Owned[$processId] = $item.StartTime.Ticks
      $details += [pscustomobject]@{
        processId = $processId; name = $item.ProcessName
        workingSetBytes = $item.WorkingSet64; privateBytes = $item.PrivateMemorySize64
      }
    } catch { } finally { if ($item) { $item.Dispose() } }
  }
  if (-not @($details | Where-Object processId -eq $RootId).Count) { throw 'Benchmark app disappeared during sampling.' }
  [pscustomobject]@{
    processCount = $details.Count
    workingSetBytes = [long](($details | Measure-Object workingSetBytes -Sum).Sum)
    privateBytes = [long](($details | Measure-Object privateBytes -Sum).Sum)
    processes = $details
  }
}

function Median($Values) {
  $sorted = @($Values | Sort-Object)
  $middle = [int][Math]::Floor($sorted.Count / 2)
  if ($sorted.Count % 2) { return [double]$sorted[$middle] }
  return ([double]$sorted[$middle - 1] + [double]$sorted[$middle]) / 2
}

function Read-Phase([int]$ProcessId, [hashtable]$Owned) {
  Start-Sleep -Seconds 2
  $samples = @()
  foreach ($sampleIndex in 1..5) {
    $samples += Read-ProcessTree $ProcessId $Owned
    if ($sampleIndex -lt 5) { Start-Sleep -Milliseconds 200 }
  }
  [pscustomobject]@{
    workingSetMiB = [Math]::Round((Median $samples.workingSetBytes) / 1MB, 2)
    privateMiB = [Math]::Round((Median $samples.privateBytes) / 1MB, 2)
    processCount = (Median $samples.processCount)
    samples = $samples
  }
}

function Run-App($Application, [int]$Index) {
  $runDirectory = Join-Path $outputPath ($Application.Name + '\run-' + $Index)
  New-Item -ItemType Directory -Path $runDirectory -Force | Out-Null
  $start = New-Object Diagnostics.ProcessStartInfo
  $start.FileName = $Application.Path
  $start.Arguments = '--benchmark "' + $runDirectory + '"'
  $start.WorkingDirectory = Split-Path -Parent $Application.Path
  $start.UseShellExecute = $false
  $start.CreateNoWindow = $true
  $start.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
  $start.EnvironmentVariables['APPDATA'] = Join-Path $runDirectory 'roaming'
  $start.EnvironmentVariables['LOCALAPPDATA'] = Join-Path $runDirectory 'local'
  $start.EnvironmentVariables['ANNOTATE_UI_TEST'] = '1'
  $start.EnvironmentVariables['ANNOTATE_DATA_DIR'] = Join-Path $runDirectory 'data'
  $watch = [Diagnostics.Stopwatch]::StartNew()
  $process = [Diagnostics.Process]::Start($start)
  $owned = @{}
  $owned[$process.Id] = $process.StartTime.Ticks
  try {
    $ready = Wait-Report (Join-Path $runDirectory 'ready.json') $process
    $windowVisible = $ready.windowVisible
    if ($Application.Name -eq 'native') {
      $shown = Wait-Report (Join-Path $runDirectory 'shown.json') $process
      $windowVisible = $shown.visible
    }
    if (-not $windowVisible) { throw 'The app did not show its benchmark window.' }
    $watch.Stop()
    $createdToReadyMs = [long]$ready.readyMs - ([DateTimeOffset]$process.StartTime.ToUniversalTime()).ToUnixTimeMilliseconds()
    $open = Read-Phase $process.Id $owned
    [IO.File]::WriteAllText((Join-Path $runDirectory 'hide.request'), '')
    $null = Wait-Report (Join-Path $runDirectory 'hidden.json') $process
    $tray = Read-Phase $process.Id $owned
    $result = [pscustomobject]@{
      application = $Application.Name; run = $Index
      uiReadyMs = $ready.uiReadyMs; processCreatedToReadyMs = $createdToReadyMs
      launchToReadyObservedMs = [Math]::Round($watch.Elapsed.TotalMilliseconds, 1)
      windowVisible = $windowVisible; ready = $ready; open = $open; tray = $tray
    }
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $runDirectory 'sample.json') -Encoding UTF8
    return $result
  } finally {
    [IO.File]::WriteAllText((Join-Path $runDirectory 'quit.request'), '')
    if (-not $process.WaitForExit(10000)) { $process.Kill(); $process.WaitForExit() }
    $process.Dispose()
    # Only terminate leaked benchmark children with the same PID AND creation time.
    foreach ($processId in @($owned.Keys)) {
      $child = $null
      try {
        $child = [Diagnostics.Process]::GetProcessById($processId)
        if ($child.StartTime.Ticks -eq $owned[$processId] -and -not $child.WaitForExit(3000)) { $child.Kill(); $child.WaitForExit() }
      } catch { } finally { if ($child) { $child.Dispose() } }
    }
  }
}

# One untimed launch warms binaries/fonts and creates the isolated WebView profile.
foreach ($application in $applications) { $null = Run-App $application 0 }
$runsData = @()
foreach ($index in 1..$Runs) {
  foreach ($application in $applications) { $runsData += Run-App $application $index }
}
$summary = @()
foreach ($application in $applications) {
  $data = @($runsData | Where-Object application -eq $application.Name)
  $summary += [pscustomobject]@{
    application = $application.Name; runs = $Runs
    uiReadyMs = (Median $data.uiReadyMs)
    processCreatedToReadyMs = (Median $data.processCreatedToReadyMs)
    openWorkingSetMiB = (Median $data.open.workingSetMiB)
    openPrivateMiB = (Median $data.open.privateMiB)
    trayWorkingSetMiB = (Median $data.tray.workingSetMiB)
    trayPrivateMiB = (Median $data.tray.privateMiB)
    openProcesses = (Median $data.open.processCount)
    trayProcesses = (Median $data.tray.processCount)
  }
}
$report = [pscustomobject]@{
  measuredUtc = [DateTime]::UtcNow.ToString('o')
  operatingSystem = [Environment]::OSVersion.VersionString
  processor = (Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Name)
  executables = @($applications | ForEach-Object { [pscustomobject]@{ name = $_.Name; path = $_.Path; sha256 = (Get-FileHash -LiteralPath $_.Path -Algorithm SHA256).Hash } })
  conditions = 'Warm filesystem cache; reused isolated WebView profile; empty Home/default settings; exact offline Satoshi fonts; no credentials, microphone, models, hotkeys or network bootstrap; Tauri capture-device override watcher disabled. Ready means completed native render or DOM + fonts + two animation frames. Each memory phase waits 2s then takes 5 process-tree samples. Working set sums include shared pages; private bytes count committed private memory, not physical RAM.'
  summary = $summary; runs = $runsData
}
$reportPath = Join-Path $outputPath 'results.json'
$report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $reportPath -Encoding UTF8
$summary | Format-Table -AutoSize
Write-Output "Report: $reportPath"

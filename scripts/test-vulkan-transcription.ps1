param(
  [string]$Worker = (Join-Path $PSScriptRoot '..\src-tauri\binaries\whisper-worker-vulkan-x86_64-pc-windows-msvc.exe'),
  [string]$DataDirectory = (Join-Path $env:LOCALAPPDATA 'com.annotate.app'),
  [ValidateRange(1, 600)][int]$TimeoutSeconds = 180
)
$ErrorActionPreference = 'Stop'
$workerPath = (Resolve-Path -LiteralPath $Worker).Path
$modelDirectory = (Resolve-Path -LiteralPath $DataDirectory).Path
$modelPath = Join-Path $modelDirectory 'ggml-large-v3-turbo-q5_0.bin'
if (-not (Test-Path -LiteralPath $modelPath -PathType Leaf)) { throw "Existing local model required: $modelPath" }
$runDirectory = Join-Path ([IO.Path]::GetTempPath()) ('annotate-vulkan-inference-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runDirectory | Out-Null
$copiedWorker = Join-Path $runDirectory 'whisper-worker-vulkan.exe'
Copy-Item -LiteralPath $workerPath -Destination $copiedWorker
$wavPath = Join-Path $runDirectory 'synthetic-speech.wav'
$phrase = 'The quick brown fox jumps over the lazy dog. This offline speech test verifies native Windows transcription.'
$report = [ordered]@{
  passed = $false
  artifacts = $runDirectory
  worker = $workerPath
  workerSha256 = (Get-FileHash -LiteralPath $copiedWorker -Algorithm SHA256).Hash
  model = $modelPath
  inputText = $phrase
}
$process = $null
$stderrTask = $null
$failure = $null

function Invoke-WorkerCommand([hashtable]$Command) {
  $process.StandardInput.WriteLine(($Command | ConvertTo-Json -Compress -Depth 5))
  $process.StandardInput.Flush()
  $pending = $process.StandardOutput.ReadLineAsync()
  if (-not $pending.Wait($TimeoutSeconds * 1000)) { throw "Worker command timed out: $($Command.cmd)" }
  $line = $pending.GetAwaiter().GetResult()
  if ($null -eq $line) { throw "Worker exited before replying: $($Command.cmd)" }
  $response = $line | ConvertFrom-Json
  if (-not $response.ok) { throw "Worker command $($Command.cmd) failed: $($response.error)" }
  return $response
}

try {
  # Offline SAPI speech goes directly to a WAV file, never to a microphone or speaker.
  $voice = $null
  $stream = $null
  $voices = $null
  try {
    $voice = New-Object -ComObject SAPI.SpVoice
    $voices = $voice.GetVoices('Language=409', '')
    if ($voices.Count -eq 0) { throw 'An installed English (US) SAPI voice is required.' }
    $voice.Voice = $voices.Item(0)
    $report.voice = $voice.Voice.GetDescription()
    $stream = New-Object -ComObject SAPI.SpFileStream
    $stream.Format.Type = 18 # SAFT16kHz16BitMono
    $stream.Open($wavPath, 3, $false) # SSFMCreateForWrite
    $voice.AudioOutputStream = $stream
    $voice.Volume = 100
    $voice.Rate = 0
    $null = $voice.Speak($phrase, 0)
    $stream.Close()
  } finally {
    if ($null -ne $stream) {
      try { $stream.Close() } catch {}
      $null = [Runtime.InteropServices.Marshal]::FinalReleaseComObject($stream)
    }
    if ($null -ne $voice) { $null = [Runtime.InteropServices.Marshal]::FinalReleaseComObject($voice) }
    if ($null -ne $voices) { $null = [Runtime.InteropServices.Marshal]::FinalReleaseComObject($voices) }
  }
  $audio = [IO.File]::ReadAllBytes($wavPath)
  if ($audio.Length -le 44) { throw 'SAPI did not produce a nonempty WAV file.' }
  $audioBase64 = [Convert]::ToBase64String($audio)
  $info = New-Object Diagnostics.ProcessStartInfo
  $info.FileName = $copiedWorker
  $info.WorkingDirectory = $runDirectory
  $info.UseShellExecute = $false
  $info.CreateNoWindow = $true
  $info.RedirectStandardInput = $true
  $info.RedirectStandardOutput = $true
  $info.RedirectStandardError = $true
  $info.StandardOutputEncoding = New-Object Text.UTF8Encoding($false)
  $info.StandardErrorEncoding = New-Object Text.UTF8Encoding($false)
  $process = [Diagnostics.Process]::Start($info)
  $report.workerPid = $process.Id
  $stderrTask = $process.StandardError.ReadToEndAsync()
  Write-Host "Synthetic inference artifacts: $runDirectory"
  $report.backend = (Invoke-WorkerCommand @{ cmd = 'backend' }).backend
  if ($report.backend -ne 'vulkan') { throw 'Expected the Vulkan worker.' }
  $timer = [Diagnostics.Stopwatch]::StartNew()
  $null = Invoke-WorkerCommand @{ cmd = 'load'; data_dir = $modelDirectory }
  $report.loadSeconds = [Math]::Round($timer.Elapsed.TotalSeconds, 3)
  Write-Host 'Read-only model loaded; transcribing synthetic speech...'
  $timer.Restart()
  $transcription = Invoke-WorkerCommand @{ cmd = 'transcribe'; audio_b64 = $audioBase64; language = 'en'; prompt = '' }
  $report.transcribeSeconds = [Math]::Round($timer.Elapsed.TotalSeconds, 3)
  $report.transcription = $transcription.text
  $words = (($transcription.text.ToLowerInvariant() -replace '[^a-z ]', ' ') -split '\s+')
  $missing = @(@('quick', 'brown', 'fox', 'lazy', 'dog', 'offline', 'speech', 'windows', 'transcription') | Where-Object { $_ -notin $words })
  if ($missing.Count) { throw "Expected words missing: $($missing -join ', '); transcript: $($transcription.text)" }
  $timer.Restart()
  $segments = Invoke-WorkerCommand @{ cmd = 'transcribe_segments'; audio_b64 = $audioBase64; language = 'en'; prompt = '' }
  $report.segmentSeconds = [Math]::Round($timer.Elapsed.TotalSeconds, 3)
  $report.segments = @($segments.segments)
  if ($report.segments.Count -eq 0) { throw 'Timestamped transcription returned no segments.' }
  foreach ($segment in $report.segments) {
    if ($segment.start -lt 0 -or $segment.end -le $segment.start -or [string]::IsNullOrWhiteSpace($segment.text)) {
      throw 'Timestamped transcription returned an invalid segment.'
    }
  }
  $null = Invoke-WorkerCommand @{ cmd = 'quit' }
  $process.StandardInput.Close()
  if (-not $process.WaitForExit(10000)) { throw 'Worker did not exit after quit.' }
  if ($process.ExitCode -ne 0) { throw "Worker exit code: $($process.ExitCode)" }
  $stderr = $stderrTask.GetAwaiter().GetResult()
  if ($stderr -notmatch '(?i)using Vulkan\d* backend') { throw 'Worker logs did not confirm an active Vulkan GPU backend.' }
  $report.vulkanEvidence = @($stderr -split '\r?\n' | Where-Object { $_ -match '(?i)(ggml_vulkan:|using Vulkan\d* backend|Vulkan\d+.*buffer)' })
  $report.passed = $true
} catch {
  $failure = $_.Exception.Message
  $report.error = $failure
} finally {
  if ($null -ne $process) {
    if (-not $process.HasExited) {
      try { $process.StandardInput.WriteLine('{"cmd":"quit"}'); $process.StandardInput.Close() } catch {}
      if (-not $process.WaitForExit(3000)) {
        try { $process.Kill() } catch { if (-not $process.HasExited) { $report.cleanupError = $_.Exception.Message } }
        if (-not $process.WaitForExit(10000)) { $report.cleanupError = 'Owned worker did not exit after Kill.' }
      }
    }
    $report.workerExited = $process.HasExited
    if ($process.HasExited) { $report.exitCode = $process.ExitCode }
    if ($null -ne $stderrTask) {
      try {
        if ($stderrTask.Wait(2000)) { [IO.File]::WriteAllText((Join-Path $runDirectory 'worker-stderr.log'), $stderrTask.GetAwaiter().GetResult()) }
      } catch {}
    }
    $process.Dispose()
  }
  $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $runDirectory 'report.json') -Encoding UTF8
}
if ($null -ne $failure) { throw "$failure; artifacts: $runDirectory" }
Write-Host "PASS: $($report.transcription)"
Write-Host "Vulkan GPU inference, timestamped segments, and clean worker exit verified. Report: $(Join-Path $runDirectory 'report.json')"

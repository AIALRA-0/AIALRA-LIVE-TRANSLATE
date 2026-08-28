$ErrorActionPreference = "Stop" # A model-worker startup failure prevents an unhealthy agent from claiming jobs.
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path # Resolve one exact checkout.
$shellPath = (Get-Process -Id $PID).Path # Child scripts use the same PowerShell runtime.
$workerScript = '"{0}"' -f (Join-Path $PSScriptRoot "run-worker.ps1") # Quote the path because the workspace contains spaces.
$worker = Start-Process -FilePath $shellPath -ArgumentList @("-NoProfile", "-File", $workerScript) -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru # Start loopback-only inference APIs.
try {
    $deadline = (Get-Date).AddSeconds(120) # CUDA model environment receives a bounded readiness period.
    do {
        try { $ready = (Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:8790/health" -TimeoutSec 3).StatusCode -eq 200 } catch { $ready = $false } # Health does not load course data.
        if ($ready) { break } # Begin leasing only after the local provider is reachable.
        Start-Sleep -Seconds 1 # Avoid a busy loop during Python startup.
    } while ((Get-Date) -lt $deadline)
    if (!$ready) { throw "本机模型 Worker 未在 120 秒内就绪" } # Scheduled Task records a visible non-zero result.
    & (Join-Path $PSScriptRoot "run-gpu-agent.ps1") # Keep the task alive while the authenticated agent runs.
} finally {
    $process = Get-CimInstance Win32_Process -Filter "ProcessId = $($worker.Id)" -ErrorAction SilentlyContinue # Verify the exact child remains before stopping it.
    if ($null -ne $process -and $process.CommandLine -like "*run-worker.ps1*") { & taskkill.exe /PID $worker.Id /T /F | Out-Null } # Stop only this task-owned worker tree.
}

$ErrorActionPreference = "Stop" # A provider failure restarts the complete local stack instead of claiming jobs indefinitely.
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path # Resolve one exact checkout.
$shellPath = (Get-Process -Id $PID).Path # Child scripts use the same PowerShell runtime.
$workerScript = '"{0}"' -f (Join-Path $PSScriptRoot "run-worker.ps1") # Quote paths because the workspace may contain spaces.
$agentScript = '"{0}"' -f (Join-Path $PSScriptRoot "run-gpu-agent.ps1")
$restartDelaySeconds = 1
$ollamaUrl = if ([string]::IsNullOrWhiteSpace($env:AIALRA_OLLAMA_URL)) { "http://127.0.0.1:11434" } else { $env:AIALRA_OLLAMA_URL.TrimEnd("/") }
$ollamaModel = if ([string]::IsNullOrWhiteSpace($env:AIALRA_OLLAMA_MODEL)) { "qwen2.5:3b-instruct" } else { $env:AIALRA_OLLAMA_MODEL }
$explanationModel = if ([string]::IsNullOrWhiteSpace($env:AIALRA_EXPLANATION_MODEL)) { "qwen2.5:3b-instruct" } else { $env:AIALRA_EXPLANATION_MODEL }
$summaryModel = if ([string]::IsNullOrWhiteSpace($env:AIALRA_SUMMARY_MODEL)) { "qwen2.5:7b-instruct" } else { $env:AIALRA_SUMMARY_MODEL }
$visionModel = if ([string]::IsNullOrWhiteSpace($env:AIALRA_VISION_MODEL)) { "qwen3-vl:8b-instruct" } else { $env:AIALRA_VISION_MODEL }
$requiredModels = @($ollamaModel, $explanationModel, $summaryModel, $visionModel) | Select-Object -Unique

function Test-OllamaReady {
    try {
        $tags = Invoke-RestMethod -Uri "$ollamaUrl/api/tags" -TimeoutSec 5
        $availableModels = @($tags.models | ForEach-Object name)
        return @($requiredModels | Where-Object { $_ -notin $availableModels }).Count -eq 0
    } catch {
        return $false
    }
}

function Start-OwnedOllama {
    if (Test-OllamaReady) { return $null }
    $command = Get-Command "ollama.exe" -ErrorAction Stop
    $process = Start-Process -FilePath $command.Source -ArgumentList @("serve") -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru
    $deadline = (Get-Date).AddSeconds(60)
    do {
        if ($process.HasExited) { throw "Ollama exited before its local API became ready" }
        if (Test-OllamaReady) { return $process }
        Start-Sleep -Seconds 1
        $process.Refresh()
    } while ((Get-Date) -lt $deadline)
    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    throw "Ollama did not become ready within 60 seconds"
}

function Initialize-LocalProviders {
    # Load ASR before the agent can lease production audio, then load the GPU LLM,
    # and run ASR once more after GPU initialization has settled.
    $silentPcm = [Convert]::ToBase64String([byte[]]::new(32000))
    $asrBody = @{
        pcm_s16le_base64 = $silentPcm
        sample_rate = 16000
        language = "en"
        initial_prompt = ""
    } | ConvertTo-Json -Compress
    [void](Invoke-RestMethod -Uri "http://127.0.0.1:8790/v1/asr/transcribe" -Method Post -ContentType "application/json" -Body $asrBody -TimeoutSec 120)

    $ollamaBody = @{
        model = $ollamaModel
        prompt = "Reply OK"
        stream = $false
        keep_alive = -1
        options = @{ num_predict = 2; temperature = 0 }
    } | ConvertTo-Json -Depth 4 -Compress
    [void](Invoke-RestMethod -Uri "$ollamaUrl/api/generate" -Method Post -ContentType "application/json" -Body $ollamaBody -TimeoutSec 120)
    [void](Invoke-RestMethod -Uri "http://127.0.0.1:8790/v1/asr/transcribe" -Method Post -ContentType "application/json" -Body $asrBody -TimeoutSec 120)
}

function Stop-OwnedOllama([Diagnostics.Process]$Process) {
    if ($null -eq $Process) { return }
    $record = Get-CimInstance Win32_Process -Filter "ProcessId = $($Process.Id)" -ErrorAction SilentlyContinue
    if ($null -ne $record -and $record.Name -ieq "ollama.exe" -and $record.CommandLine -like "*serve*") {
        & taskkill.exe /PID $Process.Id /T /F | Out-Null
    }
}

function Set-ModelWorkerPriority([Diagnostics.Process]$WorkerWrapper) {
    # Realtime audio recognition must win ordinary desktop CPU contention without changing
    # processor affinity or lowering the priority of user-owned applications
    $WorkerWrapper.PriorityClass = [Diagnostics.ProcessPriorityClass]::AboveNormal
    Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Where-Object {
            $_.CommandLine -like "*$projectRoot*" -and
            $_.CommandLine -like "*uvicorn workers.model_worker.main:app*"
        } |
        ForEach-Object {
            $modelWorker = Get-Process -Id $_.ProcessId -ErrorAction SilentlyContinue
            if ($modelWorker) { $modelWorker.PriorityClass = [Diagnostics.ProcessPriorityClass]::AboveNormal }
        }
}

function Stop-OwnedProcessTree([Diagnostics.Process]$Process, [string]$ExpectedScript, [string]$ExpectedModule) {
    if ($null -eq $Process) { return }
    $record = Get-CimInstance Win32_Process -Filter "ProcessId = $($Process.Id)" -ErrorAction SilentlyContinue
    if ($null -ne $record -and $record.CommandLine -like "*$ExpectedScript*") {
        & taskkill.exe /PID $Process.Id /T /F | Out-Null
        return
    }

    # A Windows launcher can exit before its Python child. WMI retains the old parent ID,
    # so remove only descendants that match both this checkout and the expected module.
    $allProcesses = @(Get-CimInstance Win32_Process -ErrorAction SilentlyContinue)
    $pendingParents = [Collections.Generic.Queue[uint32]]::new()
    $pendingParents.Enqueue([uint32]$Process.Id)
    while ($pendingParents.Count -gt 0) {
        $parentId = $pendingParents.Dequeue()
        foreach ($child in @($allProcesses | Where-Object ParentProcessId -eq $parentId)) {
            $pendingParents.Enqueue([uint32]$child.ProcessId)
            if ($child.CommandLine -like "*$projectRoot*" -and $child.CommandLine -like "*$ExpectedModule*") {
                Stop-Process -Id $child.ProcessId -Force -ErrorAction SilentlyContinue
            }
        }
    }
}

while ($true) {
    $worker = $null
    $agent = $null
    $ownedOllama = $null
    try {
        $ownedOllama = Start-OwnedOllama
        $worker = Start-Process -FilePath $shellPath -ArgumentList @("-NoProfile", "-File", $workerScript) -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru
        $deadline = (Get-Date).AddSeconds(120)
        $ready = $false
        do {
            if ($worker.HasExited) { throw "本机模型 Worker 在就绪前退出" }
            try {
                $health = Invoke-RestMethod -Uri "http://127.0.0.1:8790/health" -TimeoutSec 3
                $ready = $health.asr_available -and $health.ollama_available
            } catch { $ready = $false }
            if ($ready) { break }
            Start-Sleep -Seconds 1
        } while ((Get-Date) -lt $deadline)
        if (!$ready) { throw "本机模型 Worker 未在 120 秒内就绪" }
        Set-ModelWorkerPriority $worker
        Initialize-LocalProviders

        $agent = Start-Process -FilePath $shellPath -ArgumentList @("-NoProfile", "-File", $agentScript) -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru
        $restartDelaySeconds = 1
        $ollamaFailures = 0
        while (!$worker.HasExited -and !$agent.HasExited) {
            Start-Sleep -Seconds 2
            $worker.Refresh()
            $agent.Refresh()
            if ($ownedOllama) {
                $ownedOllama.Refresh()
                if ($ownedOllama.HasExited) { throw "Owned Ollama process exited" }
            }
            if (Test-OllamaReady) {
                $ollamaFailures = 0
            } else {
                $ollamaFailures += 1
                if ($ollamaFailures -ge 3) { throw "Ollama local API remained unavailable" }
            }
        }
        if ($worker.HasExited) { throw "本机模型 Worker 意外退出" }
        throw "本机 GPU Agent 意外退出"
    } catch {
        Write-Warning $_.Exception.Message
    } finally {
        Stop-OwnedProcessTree $agent "run-gpu-agent.ps1" "workers.gpu_agent.main"
        Stop-OwnedProcessTree $worker "run-worker.ps1" "workers.model_worker.main:app"
        Stop-OwnedOllama $ownedOllama
    }
    Start-Sleep -Seconds $restartDelaySeconds
    $restartDelaySeconds = [Math]::Min(30, $restartDelaySeconds * 2)
}

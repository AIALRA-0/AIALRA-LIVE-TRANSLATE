param(
    [ValidateRange(1, 120)][int]$Minutes = 30,
    [ValidateRange(5, 120)][int]$IntervalSeconds = 30,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$SshTarget,
    [switch]$IncludeTestBrowser
)

$ErrorActionPreference = "Stop"
$samples = [System.Collections.Generic.List[object]]::new()
$deadline = (Get-Date).AddMinutes($Minutes)

function Convert-MemoryToMiB([string]$value) {
    if ($value -match '([0-9.]+)([KMG]iB)') {
        $amount = [double]$Matches[1]
        switch ($Matches[2]) {
            'KiB' { return $amount / 1024 }
            'MiB' { return $amount }
            'GiB' { return $amount * 1024 }
        }
    }
    return $null
}

while ((Get-Date) -lt $deadline) {
    $workerIds = Get-CimInstance Win32_Process -Filter "Name='python.exe'" |
        Where-Object { $_.CommandLine -match 'workers.gpu_agent.main|uvicorn workers.model_worker.main' } |
        Select-Object -ExpandProperty ProcessId
    $localBytes = 0L
    foreach ($processId in $workerIds) {
        $process = Get-Process -Id $processId -ErrorAction SilentlyContinue
        if ($process) { $localBytes += $process.WorkingSet64 }
    }
    Get-Process -Name 'ollama','ollama_llama_server' -ErrorAction SilentlyContinue |
        ForEach-Object { $localBytes += $_.WorkingSet64 }
    $browserBytes = 0L
    if ($IncludeTestBrowser) {
        $browserIds = Get-CimInstance Win32_Process |
            Where-Object { $_.CommandLine -match '--use-file-for-fake-audio-capture=' } |
            Select-Object -ExpandProperty ProcessId
        foreach ($browserId in $browserIds) {
            $browser = Get-Process -Id $browserId -ErrorAction SilentlyContinue
            if ($browser) { $browserBytes += $browser.WorkingSet64 }
        }
    }
    $gpuUsed = (& nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits 2>$null | Select-Object -First 1)
    $remoteUsage = (& ssh $SshTarget 'docker stats --no-stream --format "{{.MemUsage}}" aialra-live-translate-core-1' 2>$null)
    $cgroupStats = @(& ssh $SshTarget 'docker exec aialra-live-translate-core-1 cat /sys/fs/cgroup/memory.stat' 2>$null)
    $coreAnonBytes = ($cgroupStats | Where-Object { $_ -match '^anon\s+([0-9]+)$' } | ForEach-Object { [long]$Matches[1] } | Select-Object -First 1)
    $coreFileBytes = ($cgroupStats | Where-Object { $_ -match '^file\s+([0-9]+)$' } | ForEach-Object { [long]$Matches[1] } | Select-Object -First 1)
    $coreInactiveFileBytes = ($cgroupStats | Where-Object { $_ -match '^inactive_file\s+([0-9]+)$' } | ForEach-Object { [long]$Matches[1] } | Select-Object -First 1)
    $samples.Add([pscustomobject]@{
        time = (Get-Date).ToUniversalTime().ToString('o')
        local_worker_mib = [math]::Round($localBytes / 1MB, 2)
        test_browser_mib = if ($IncludeTestBrowser) { [math]::Round($browserBytes / 1MB, 2) } else { $null }
        gpu_mib = if ($gpuUsed -match '^\s*([0-9]+)') { [int]$Matches[1] } else { $null }
        core_mib = Convert-MemoryToMiB (($remoteUsage -split '/')[0].Trim())
        core_anon_mib = if ($null -ne $coreAnonBytes) { [math]::Round($coreAnonBytes / 1MB, 2) } else { $null }
        core_file_mib = if ($null -ne $coreFileBytes) { [math]::Round($coreFileBytes / 1MB, 2) } else { $null }
        core_inactive_file_mib = if ($null -ne $coreInactiveFileBytes) { [math]::Round($coreInactiveFileBytes / 1MB, 2) } else { $null }
    })
    Start-Sleep -Seconds $IntervalSeconds
}

function Metric-Summary([string]$name) {
    $values = @($samples | ForEach-Object { $_.$name } | Where-Object { $null -ne $_ })
    if (!$values) { return $null }
    $count = $values.Count
    $sumX = (($count - 1) * $count) / 2
    $sumXX = (($count - 1) * $count * ((2 * $count) - 1)) / 6
    $sumY = ($values | Measure-Object -Sum).Sum
    $sumXY = 0.0
    for ($index = 0; $index -lt $count; $index++) { $sumXY += $index * $values[$index] }
    $denominator = ($count * $sumXX) - ($sumX * $sumX)
    $slopePerSample = if ($denominator -eq 0) { 0 } else { (($count * $sumXY) - ($sumX * $sumY)) / $denominator }
    [pscustomobject]@{
        first = $values[0]
        last = $values[-1]
        min = ($values | Measure-Object -Minimum).Minimum
        max = ($values | Measure-Object -Maximum).Maximum
        growth = [math]::Round($values[-1] - $values[0], 2)
        trend_mib_per_hour = [math]::Round($slopePerSample * (3600 / $IntervalSeconds), 2)
    }
}

[pscustomobject]@{
    status = 'COMPLETE'
    duration_minutes = $Minutes
    samples = $samples.Count
    local_worker_mib = Metric-Summary 'local_worker_mib'
    test_browser_mib = Metric-Summary 'test_browser_mib'
    gpu_mib = Metric-Summary 'gpu_mib'
    core_mib = Metric-Summary 'core_mib'
    core_anon_mib = Metric-Summary 'core_anon_mib'
    core_file_mib = Metric-Summary 'core_file_mib'
    core_inactive_file_mib = Metric-Summary 'core_inactive_file_mib'
} | ConvertTo-Json -Depth 4

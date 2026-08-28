param(
    [switch]$NoBrowser # Automation and test runs can start services without opening another page.
)

$ErrorActionPreference = "Stop" # A partial startup returns a clear failure instead of a misleading ready message.
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path # Anchor every mutable path inside the project.
$runDirectory = Join-Path $projectRoot "data\run" # PID files are local runtime state and remain outside version control.
New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null # Create the bounded runtime directory when absent.

# Load simple KEY=VALUE entries from the ignored local file without executing its contents as PowerShell.
$localEnvFile = Join-Path $projectRoot ".env" # The checked-in example documents keys while this file holds machine-specific values.
if (Test-Path -LiteralPath $localEnvFile -PathType Leaf) {
    foreach ($rawLine in Get-Content -LiteralPath $localEnvFile -Encoding UTF8) {
        $trimmedLine = $rawLine.Trim() # Ignore surrounding whitespace while preserving spaces inside values.
        if ($trimmedLine.Length -eq 0 -or $trimmedLine.StartsWith("#")) { continue } # Empty lines and comments do not define variables.
        $separatorIndex = $trimmedLine.IndexOf("=") # Only the first equals sign separates the key from its value.
        if ($separatorIndex -le 0) { throw "本机 .env 包含无效配置行：$trimmedLine" } # Malformed configuration stops before any service starts.
        $key = $trimmedLine.Substring(0, $separatorIndex).Trim() # Extract one explicit environment variable name.
        $value = $trimmedLine.Substring($separatorIndex + 1).Trim() # Preserve additional equals signs inside tokens or URLs.
        if ($key -notmatch '^[A-Za-z_][A-Za-z0-9_]*$') { throw "本机 .env 包含无效变量名：$key" } # Restrict names to portable environment syntax.
        if ($value.Length -ge 2 -and (($value.StartsWith('"') -and $value.EndsWith('"')) -or ($value.StartsWith("'") -and $value.EndsWith("'")))) { $value = $value.Substring(1, $value.Length - 2) } # Remove one matching quote pair without evaluating the value.
        [Environment]::SetEnvironmentVariable($key, $value, "Process") # Child services inherit the validated process-local setting.
    }
}

# A local-only run receives an ephemeral shared token when the user did not configure one.
if ([string]::IsNullOrWhiteSpace($env:AIALRA_WORKER_TOKEN)) {
    $randomBytes = [byte[]]::new(32) # Use cryptographic randomness without writing the raw token to disk or terminal.
    [Security.Cryptography.RandomNumberGenerator]::Fill($randomBytes) # Generate one process-scoped bearer token.
    $env:AIALRA_WORKER_TOKEN = [Convert]::ToHexString($randomBytes).ToLowerInvariant() # Child services inherit the same temporary secret.
}
$tokenBytes = [Text.Encoding]::UTF8.GetBytes($env:AIALRA_WORKER_TOKEN) # Hash the token for the core-side verifier.
$tokenHash = [Security.Cryptography.SHA256]::HashData($tokenBytes) # The core stores only the digest in its process environment.
$env:AIALRA_WORKER_TOKEN_SHA256 = [Convert]::ToHexString($tokenHash).ToLowerInvariant() # Match the Rust hexadecimal verifier.

# Refuse to overwrite PID records when either project port already has a listener.
$occupiedProjectPorts = @(Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object { $_.LocalPort -in @(8787, 8790) } | Select-Object -ExpandProperty LocalPort -Unique) # Resolve only the two fixed loopback service ports.
if ($occupiedProjectPorts.Count -gt 0) { throw "AIALRA 端口已被占用：$($occupiedProjectPorts -join ', ')，请先停止现有服务。" } # A stale or unrelated listener must be handled before startup.

# Install locked dependencies and rebuild the browser bundle before launching long-running services.
Push-Location -LiteralPath $projectRoot # Package commands expect the repository root.
try {
    uv sync --extra dev --extra speech # Restore Python, ASR, and project-local NVIDIA libraries.
    pnpm install --frozen-lockfile # Restore the exact browser dependency graph.
    pnpm build # Produce the static application served by the Rust process.
} finally {
    Pop-Location # Restore the caller's working directory even after a build failure.
}

$shellPath = (Get-Process -Id $PID).Path # Child launchers use the same PowerShell runtime as this script.
$workerScript = '"{0}"' -f (Join-Path $PSScriptRoot "run-worker.ps1") # Quote the script path because the workspace contains spaces.
$worker = Start-Process -FilePath $shellPath -ArgumentList @("-NoProfile", "-File", $workerScript) -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru # Launch the loopback model worker.
$worker.Id | Set-Content -LiteralPath (Join-Path $runDirectory "worker.pid") -Encoding ascii # Record the exact child PID for safe shutdown.
$coreScript = '"{0}"' -f (Join-Path $PSScriptRoot "run-core.ps1") # Quote the second script path with the same rule.
$core = Start-Process -FilePath $shellPath -ArgumentList @("-NoProfile", "-File", $coreScript) -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru # Launch the durable local core.
$core.Id | Set-Content -LiteralPath (Join-Path $runDirectory "core.pid") -Encoding ascii # Record the exact child PID for safe shutdown.

# Readiness requires both the worker and the core to answer before the browser opens.
$deadline = (Get-Date).AddSeconds(120) # First compilation and model environment checks receive a bounded startup window.
do {
    $workerReady = $false # Each attempt independently checks the worker.
    $coreReady = $false # Each attempt independently checks the control service.
    try { $workerReady = (Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:8790/health" -TimeoutSec 2).StatusCode -eq 200 } catch { $workerReady = $false } # Worker readiness includes optional provider flags.
    try { $coreReady = (Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:8787/api/v1/health" -TimeoutSec 2).StatusCode -eq 200 } catch { $coreReady = $false } # Core readiness confirms storage and routing.
    if ($workerReady -and $coreReady) { break } # Both services must be available to claim startup success.
    Start-Sleep -Milliseconds 750 # Short polling avoids busy waiting during compilation.
} while ((Get-Date) -lt $deadline)

if (!$workerReady -or !$coreReady) {
    & (Join-Path $PSScriptRoot "stop-local.ps1") # Remove only the verified launchers created by this project.
    throw "本地服务未在 120 秒内就绪，请检查模型依赖和端口状态。" # Report a bounded, actionable failure after partial cleanup.
}
$agentScript = '"{0}"' -f (Join-Path $PSScriptRoot "run-gpu-agent.ps1") # The agent begins only after both local APIs are ready.
$agent = Start-Process -FilePath $shellPath -ArgumentList @("-NoProfile", "-File", $agentScript) -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru # Connect the model worker to the persistent queue.
$agent.Id | Set-Content -LiteralPath (Join-Path $runDirectory "agent.pid") -Encoding ascii # Safe shutdown verifies this launcher by name.
if (!$NoBrowser) { Start-Process "http://127.0.0.1:8787" } # Interactive runs open one page; automation stays headless.
Write-Output "AIALRA 已启动：http://127.0.0.1:8787" # Give terminal users a copyable local address.

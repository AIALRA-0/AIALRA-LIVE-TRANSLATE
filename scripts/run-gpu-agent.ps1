$ErrorActionPreference = "Stop" # Agent exits when its private token or local model gate is unavailable.
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path # Keep every runtime lookup inside this checkout.
$configuredGateway = [Environment]::GetEnvironmentVariable("AIALRA_GPU_GATEWAY_URL", "User") # Read installer configuration even before the next Windows login.
if (![string]::IsNullOrWhiteSpace($configuredGateway)) { $env:AIALRA_GPU_GATEWAY_URL = $configuredGateway } # Pass the private endpoint only to this process tree.
$secretFile = Join-Path $projectRoot "data\secrets\worker-token.dpapi" # The encrypted token is ignored and bound to this Windows user.
if (!(Test-Path -LiteralPath $secretFile -PathType Leaf) -and [string]::IsNullOrWhiteSpace($env:AIALRA_WORKER_TOKEN)) { throw "缺少本机 Worker 令牌" } # Never start an unauthenticated agent.
if ([string]::IsNullOrWhiteSpace($env:AIALRA_WORKER_TOKEN)) {
    $encryptedToken = (Get-Content -LiteralPath $secretFile -Raw).Trim() # Remove the text-file line ending without changing ciphertext.
    $secureToken = $encryptedToken | ConvertTo-SecureString # Windows DPAPI decrypts only for the installing user.
    $credential = [System.Net.NetworkCredential]::new("worker", $secureToken) # Convert only in this process before child launch.
    $env:AIALRA_WORKER_TOKEN = $credential.Password # The token remains process-local and is not written to logs.
}
Set-Location -LiteralPath $projectRoot # Python imports resolve from the repository root.
$pythonPath = Join-Path $projectRoot ".venv\Scripts\python.exe" # Keep the supervised wrapper as the direct parent of the long-running agent.
if (!(Test-Path -LiteralPath $pythonPath -PathType Leaf)) { throw "项目虚拟环境 Python 不存在" }
& $pythonPath -m workers.gpu_agent.main # Run independent ASR, translation, and explanation/document lanes.
if ($LASTEXITCODE -ne 0) { throw "本机 GPU Agent 退出，代码 $LASTEXITCODE" }

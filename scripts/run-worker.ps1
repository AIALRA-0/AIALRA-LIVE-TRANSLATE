$ErrorActionPreference = "Stop" # Any missing runtime or model dependency stops this child process visibly in logs.
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path # Resolve one stable workspace root.
$venvRoot = Join-Path $projectRoot ".venv" # NVIDIA Python wheels install their DLLs under the local environment.
$gpuBins = @( # CUDA libraries stay project-local and enter PATH only for the model worker.
    (Join-Path $venvRoot "Lib\site-packages\nvidia\cublas\bin"),
    (Join-Path $venvRoot "Lib\site-packages\nvidia\cudnn\bin"),
    (Join-Path $venvRoot "Lib\site-packages\nvidia\cuda_nvrtc\bin")
)
$env:PATH = ($gpuBins -join [IO.Path]::PathSeparator) + [IO.Path]::PathSeparator + $env:PATH # Enable CTranslate2 GPU loading.
Set-Location -LiteralPath $projectRoot # Relative model and data paths now resolve from the repository.
$pythonPath = Join-Path $venvRoot "Scripts\python.exe" # Avoid the uv launcher spawning an orphaned server process that outlives this supervised wrapper.
if (!(Test-Path -LiteralPath $pythonPath -PathType Leaf)) { throw "项目虚拟环境 Python 不存在" }
& $pythonPath -m uvicorn workers.model_worker.main:app --host 127.0.0.1 --port 8790 --no-access-log # Serve local model APIs on loopback without per-request transcript-adjacent noise.
if ($LASTEXITCODE -ne 0) { throw "本机模型 Worker 退出，代码 $LASTEXITCODE" }

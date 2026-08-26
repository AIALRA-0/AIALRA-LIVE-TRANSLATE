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
uv run uvicorn workers.model_worker.main:app --host 127.0.0.1 --port 8790 # Serve local model APIs on loopback only.

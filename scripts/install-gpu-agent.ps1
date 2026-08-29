param(
    [string]$WorkerToken,
    [string]$GatewayUrl = "http://worker-gateway.example.invalid",
    [string]$OllamaUrl = "http://127.0.0.1:11434",
    [string]$OllamaModel = "qwen2.5:3b-instruct"
)

$ErrorActionPreference = "Stop" # Installation must not leave a task with an incomplete secret.
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path # Resolve one exact checkout.
$secretDirectory = Join-Path $projectRoot "data\secrets" # Runtime secrets remain below the ignored data directory.
New-Item -ItemType Directory -Force -Path $secretDirectory | Out-Null # Create the bounded secret location.
$secretFile = Join-Path $secretDirectory "worker-token.dpapi" # Reuse an initialized secret when plaintext is not supplied.
if (![string]::IsNullOrWhiteSpace($WorkerToken)) {
    $secureToken = ConvertTo-SecureString -String $WorkerToken -AsPlainText -Force # Prepare DPAPI protection for the current user.
    $secureToken | ConvertFrom-SecureString | Set-Content -LiteralPath $secretFile -Encoding ascii # Persist only DPAPI ciphertext.
}
if (!(Test-Path -LiteralPath $secretFile -PathType Leaf)) { throw "请先运行 initialize-gpu-agent-secret.ps1" } # Never install a task that cannot authenticate.
$ollamaTags = Invoke-RestMethod -Uri "$($OllamaUrl.TrimEnd('/'))/api/tags" -TimeoutSec 10 # Fail installation before creating an unusable login task.
$availableModels = @($ollamaTags.models | ForEach-Object name)
if ($OllamaModel -notin $availableModels) { throw "Ollama 缺少模型 $OllamaModel，请先安装真实模型" }

$taskName = "AIALRA RTX GPU Agent" # Use one stable name so reinstall updates instead of duplicating tasks.
$launcher = Join-Path $PSScriptRoot "run-gpu-stack.ps1" # The scheduled action owns both loopback inference and the private queue agent.
$action = New-ScheduledTaskAction -Execute "pwsh.exe" -Argument "-NoProfile -WindowStyle Hidden -File `"$launcher`"" -WorkingDirectory $projectRoot # Keep the helper hidden after login.
$trigger = New-ScheduledTaskTrigger -AtLogOn # The task principal below restricts the trigger to this interactive identity.
$settings = New-ScheduledTaskSettingsSet -RestartCount 6 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero) # Recover from temporary Tailscale or provider outages.
[Environment]::SetEnvironmentVariable("AIALRA_GPU_GATEWAY_URL", $GatewayUrl, "User") # Store only the private endpoint, never the token.
[Environment]::SetEnvironmentVariable("AIALRA_OLLAMA_URL", $OllamaUrl, "User") # Persist the verified loopback provider for future logins.
[Environment]::SetEnvironmentVariable("AIALRA_OLLAMA_MODEL", $OllamaModel, "User") # Keep startup and the validated model on the same provider.
$currentUser = (& whoami.exe).Trim() # Resolve the exact interactive identity used by DPAPI.
try {
    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Settings $settings -User $currentUser -RunLevel Limited -Description "AIALRA local RTX model agent" -Force -ErrorAction Stop | Out-Null # Prefer Task Scheduler when the account can register tasks.
    Start-ScheduledTask -TaskName $taskName # Begin processing queued jobs immediately.
    $installMode = "任务计划程序"
} catch {
    $startupDirectory = [Environment]::GetFolderPath("Startup") # Standard-user installations use the current account's Startup folder.
    $shortcutPath = Join-Path $startupDirectory "AIALRA RTX GPU Agent.lnk" # Use one exact project-owned shortcut.
    $shortcutShell = New-Object -ComObject WScript.Shell # Windows creates a native hidden launcher without administrator rights.
    $shortcut = $shortcutShell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = "pwsh.exe"
    $shortcut.Arguments = "-NoProfile -WindowStyle Hidden -File `"$launcher`""
    $shortcut.WorkingDirectory = $projectRoot
    $shortcut.WindowStyle = 7
    $shortcut.Description = "AIALRA local RTX model agent"
    $shortcut.Save()
    Start-Process -FilePath "pwsh.exe" -ArgumentList @("-NoProfile", "-WindowStyle", "Hidden", "-File", ('"{0}"' -f $launcher)) -WorkingDirectory $projectRoot -WindowStyle Hidden # Start now rather than waiting for the next login.
    $installMode = "用户登录启动项"
}
Write-Output "AIALRA RTX GPU Agent 已通过${installMode}安装并启动"

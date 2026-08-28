$ErrorActionPreference = "Stop" # Only the exact AIALRA task and encrypted secret are removed.
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path # Resolve this checkout before deletion.
$taskName = "AIALRA RTX GPU Agent" # Match the installer-owned task exactly.
$task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue # Read before mutating scheduled state.
if ($null -ne $task) { Unregister-ScheduledTask -TaskName $taskName -Confirm:$false } # Remove only the named task.
$shortcutPath = Join-Path ([Environment]::GetFolderPath("Startup")) "AIALRA RTX GPU Agent.lnk" # Resolve the current-user fallback launcher.
if (Test-Path -LiteralPath $shortcutPath -PathType Leaf) { Remove-Item -LiteralPath $shortcutPath -Force } # Remove only the installer-owned shortcut.
$secretFile = Join-Path $projectRoot "data\secrets\worker-token.dpapi" # Resolve the single encrypted token path.
if (Test-Path -LiteralPath $secretFile -PathType Leaf) { Remove-Item -LiteralPath $secretFile -Force } # Remove recoverable local agent credentials.
Write-Output "AIALRA RTX GPU Agent 已卸载"

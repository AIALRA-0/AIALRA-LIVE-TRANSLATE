$task = Get-ScheduledTask -TaskName "AIALRA RTX GPU Agent" -ErrorAction SilentlyContinue # Inspect one project-owned task.
if ($null -ne $task) {
    $info = Get-ScheduledTaskInfo -TaskName "AIALRA RTX GPU Agent" # Read execution outcome without transcript or secret data.
    [pscustomobject]@{ InstallMode = "ScheduledTask"; State = $task.State; LastRunTime = $info.LastRunTime; LastTaskResult = $info.LastTaskResult } # Return only operational metadata.
    exit 0
}
$shortcutPath = Join-Path ([Environment]::GetFolderPath("Startup")) "AIALRA RTX GPU Agent.lnk" # Check the standard-user fallback.
if (Test-Path -LiteralPath $shortcutPath -PathType Leaf) { [pscustomobject]@{ InstallMode = "StartupShortcut"; State = "Installed" }; exit 0 } # Content and secrets remain hidden.
Write-Output "未安装"
exit 1

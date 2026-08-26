$ErrorActionPreference = "Stop" # PID validation failures remain visible and never broaden the shutdown target.
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path # Only this repository's launch records are trusted.
$runDirectory = Join-Path $projectRoot "data\run" # Runtime state is bounded to the project data directory.

# Each PID is checked against its command line before the process receives a stop signal.
foreach ($name in @("core", "worker")) {
    $pidFile = Join-Path $runDirectory "$name.pid" # Resolve one explicit PID file without globs.
    if (!(Test-Path -LiteralPath $pidFile)) { continue } # Missing records mean this script has nothing to stop.
    $recordedPid = [int](Get-Content -LiteralPath $pidFile -Raw) # Parse the exact process identifier written at startup.
    $process = Get-CimInstance Win32_Process -Filter "ProcessId = $recordedPid" -ErrorAction SilentlyContinue # Read command metadata without mutating state.
    if ($null -eq $process) {
        Remove-Item -LiteralPath $pidFile -Force # An exited launcher leaves no reusable PID record.
        continue # No process remains to stop.
    }
    $expectedScript = "run-$name.ps1" # The command line must name the matching project launcher.
    if ($process.CommandLine -notlike "*$expectedScript*") { throw "PID $recordedPid 不属于 AIALRA $name 服务，已拒绝停止。" } # Prevent stale PID reuse from stopping another program.
    & taskkill.exe /PID $recordedPid /T /F | Out-Null # Stop the verified launcher and only its service descendants.
    Remove-Item -LiteralPath $pidFile -Force # Successful shutdown clears the exact project-owned PID record.
}
Write-Output "AIALRA 本地服务停止请求已发送。" # Confirm the bounded operation to the user.

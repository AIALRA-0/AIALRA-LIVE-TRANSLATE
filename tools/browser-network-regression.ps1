param(
    [ValidateSet('short', 'screenshot', 'preflight', 'long')][string]$Profile = 'short',
    [string]$SshTarget = $env:AIALRA_TEST_SSH_TARGET,
    [string]$BaseUrl = 'http://127.0.0.1:18787',
    [string]$FixtureUrl = $env:AIALRA_AUDIO_FIXTURE_URL,
    [string]$RemoteCredentialFile = $env:AIALRA_TEST_HTPASSWD_PATH,
    [string]$ScreenshotPath
)

$ErrorActionPreference = 'Stop'
$requiredValues = @($SshTarget, $FixtureUrl, $RemoteCredentialFile)
if ($requiredValues.Where({ [string]::IsNullOrWhiteSpace($_) }).Count -gt 0) {
    throw 'AIALRA_TEST_SSH_TARGET, AIALRA_AUDIO_FIXTURE_URL, and AIALRA_TEST_HTPASSWD_PATH are required'
}
if ($FixtureUrl -notmatch '^https://') { throw 'The fixture URL must use HTTPS' }
if ($RemoteCredentialFile -notmatch '^/[A-Za-z0-9._/-]+$') { throw 'Invalid remote credential path' }
$credentialUser = 'codex-regression'
$credentialPassword = [Convert]::ToBase64String(
    [Security.Cryptography.RandomNumberGenerator]::GetBytes(24)
)

function Invoke-SshInput([string]$InputText, [string]$RemoteCommand) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = 'ssh'
    $start.ArgumentList.Add($SshTarget)
    $start.ArgumentList.Add($RemoteCommand)
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.UseShellExecute = $false
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    [void]$process.Start()
    $process.StandardInput.NewLine = "`n"
    $process.StandardInput.WriteLine($InputText)
    $process.StandardInput.Close()
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) {
        throw "Remote input command failed with exit code $($process.ExitCode): $stderr"
    }
    return $stdout.Trim()
}

function Remove-TemporaryCredential {
    & ssh $SshTarget "sed -i '\|^codex-regression:|d' $RemoteCredentialFile"
    if ($LASTEXITCODE -ne 0) { throw 'Failed to remove temporary fixture credential' }
}

function Invoke-BrowserCase(
    [string]$Subject,
    [string]$Channel,
    [int]$CaptureSeconds,
    [int]$OfflineSeconds
) {
    $env:AIALRA_TEST_SUBJECT = $Subject
    $env:AIALRA_BROWSER_CHANNEL = $Channel
    $env:AIALRA_BROWSER_CAPTURE_SECONDS = $CaptureSeconds.ToString()
    $env:AIALRA_BROWSER_OFFLINE_SECONDS = $OfflineSeconds.ToString()
    & node apps/web/tests/browser_dual_device.mjs
    if ($LASTEXITCODE -ne 0) { throw "$Subject browser regression failed" }
}

try {
    Remove-TemporaryCredential
    $credentialHash = Invoke-SshInput $credentialPassword 'openssl passwd -apr1 -stdin'
    [void](Invoke-SshInput "$credentialUser`:$credentialHash" "cat >> $RemoteCredentialFile")

    $pair = [Convert]::ToBase64String(
        [Text.Encoding]::ASCII.GetBytes("$credentialUser`:$credentialPassword")
    )
    $response = Invoke-WebRequest -Uri $FixtureUrl -Headers @{ Authorization = "Basic $pair" } -Method Head
    if ($response.StatusCode -ne 200) { throw "Fixture authentication returned $($response.StatusCode)" }

    $env:AIALRA_BROWSER_BASE_URL = $BaseUrl
    $env:AIALRA_AUDIO_FIXTURE_URL = $FixtureUrl
    $env:AIALRA_AUDIO_FIXTURE_USERNAME = $credentialUser
    $env:AIALRA_AUDIO_FIXTURE_PASSWORD = $credentialPassword

    if ($Profile -eq 'short') {
        Invoke-BrowserCase 'deployment-post-v5-chrome' 'chrome' 35 5
        Invoke-BrowserCase 'deployment-post-v5-edge' 'msedge' 35 5
        Invoke-BrowserCase 'deployment-post-v5-offline15' 'chrome' 45 15
        Invoke-BrowserCase 'deployment-post-v5-offline60' 'chrome' 80 60
    } elseif ($Profile -eq 'screenshot') {
        if ([string]::IsNullOrWhiteSpace($ScreenshotPath)) {
            $ScreenshotPath = Join-Path $PSScriptRoot '..\docs\assets\readme\real-gpu-project-sync.png'
        }
        $env:AIALRA_BROWSER_SCREENSHOT_PATH = $ScreenshotPath
        Invoke-BrowserCase 'deployment-readme-real-gpu' 'chrome' 35 5
    } elseif ($Profile -eq 'preflight') {
        Invoke-BrowserCase 'deployment-browser-30m-small-cpu-v28' 'chrome' 1800 5
    } else {
        Invoke-BrowserCase 'deployment-browser-90m-recovery-v26' 'chrome' 5400 5
    }
} finally {
    Remove-TemporaryCredential
    Remove-Item Env:AIALRA_AUDIO_FIXTURE_PASSWORD -ErrorAction SilentlyContinue
    Remove-Item Env:AIALRA_BROWSER_SCREENSHOT_PATH -ErrorAction SilentlyContinue
}

param(
    [Parameter(Mandatory)][ValidatePattern('^[A-Za-z0-9_.-]+$')][string]$SshTarget,
    [Parameter(Mandatory)][ValidateRange(1024, 65535)][int]$LocalPort,
    [ValidatePattern('^[A-Za-z0-9_.:-]+$')][string]$RemoteHost = '127.0.0.1',
    [Parameter(Mandatory)][ValidateRange(1, 65535)][int]$RemotePort
)

$ErrorActionPreference = 'Stop'
while ($true) {
    & ssh -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -N -L "${LocalPort}:${RemoteHost}:${RemotePort}" $SshTarget
    Start-Sleep -Seconds 1
}

$ErrorActionPreference = "Stop" # Partial secret initialization is rejected.
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path # Keep generated material under the ignored data directory.
$secretDirectory = Join-Path $projectRoot "data\secrets" # This directory is excluded from Git and normal logs.
New-Item -ItemType Directory -Force -Path $secretDirectory | Out-Null # Create one bounded runtime secret directory.
$randomBytes = [byte[]]::new(32) # The bearer token carries 256 bits of entropy.
[Security.Cryptography.RandomNumberGenerator]::Fill($randomBytes) # Use the Windows cryptographic generator.
$token = [Convert]::ToHexString($randomBytes).ToLowerInvariant() # Produce a transport-safe value without shell punctuation.
$secureToken = ConvertTo-SecureString -String $token -AsPlainText -Force # Prepare current-user DPAPI encryption.
$secureToken | ConvertFrom-SecureString | Set-Content -LiteralPath (Join-Path $secretDirectory "worker-token.dpapi") -Encoding ascii # Persist only ciphertext.
$hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($token))).ToLowerInvariant() # Derive the VPS verifier.
$hash | Set-Content -LiteralPath (Join-Path $secretDirectory "worker-token.sha256") -Encoding ascii -NoNewline # The digest is safe to transfer but remains runtime state.
$token = $null # Release the plaintext reference before returning.
Write-Output "GPU Agent 令牌已使用 Windows DPAPI 初始化"

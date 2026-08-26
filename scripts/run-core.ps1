$ErrorActionPreference = "Stop" # Compilation or startup failures terminate this child process.
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path # Resolve the repository once.
$env:RUST_LOG = "aialra_core_server=info,tower_http=warn" # Logs contain operational metadata and omit content bodies.
Set-Location -LiteralPath $projectRoot # Static assets and the default data directory use this root.
cargo run -p aialra-core-server # Compile when needed and serve the local control plane on port 8787.

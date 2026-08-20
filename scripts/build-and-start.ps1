<#
build-and-start.ps1

Convenience wrapper: builds the mock_mcp_server container image and then starts the full stack.
#>

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path

Write-Output "[pg-mcp-agent] Building mock_mcp_server container image (dev)..."
Push-Location $scriptRoot
try {
    docker compose build mock_mcp_server
} catch {
    Write-Warning "docker compose build failed. Ensure Docker is running and you have network access for crates.io."
}
Pop-Location

# Call the existing start script (which by default will not trigger a host cargo run)
Write-Output "[pg-mcp-agent] Starting stack (this will wait until Postgres and mock service health checks pass)..."
& "$scriptRoot\start-dev.ps1"

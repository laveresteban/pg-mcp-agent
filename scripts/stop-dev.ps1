<#
stop-dev.ps1

Stops the mock MCP server (if started by start-dev.ps1) and brings down the Docker Compose stack.
#>

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
nWrite-Output "[pg-mcp-agent] Stopping Docker Compose stack..."
Push-Location $scriptRoot
try {
    docker compose down
} catch {
    Write-Warning "Failed to run 'docker compose down'. Ensure Docker is installed and running."
}
Pop-Location

Write-Output "[pg-mcp-agent] Done."

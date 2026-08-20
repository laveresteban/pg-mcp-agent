<#
Start-dev.ps1

Brings up Docker Compose (Postgres + Adminer), waits for Postgres to become available,
and then starts the repo's mock MCP server via cargo (build+run). Saves the mock
server PID to scripts\tmp\mock_mcp_server.pid so it can be stopped with stop-dev.ps1.
#>

param(
    [switch]$NoMock  # set to skip launching the mock MCP server
)

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectRoot = $scriptRoot

Write-Output "[pg-mcp-agent] Starting Docker Compose stack..."

# Ensure Docker Compose is available
try {
    docker compose version > $null 2>&1
} catch {
    Write-Error "Docker Compose not available. Install Docker Desktop or the Docker CLI."
    exit 1
}

# Start the stack
Push-Location $projectRoot
docker compose up -d
Pop-Location

# Wait for Postgres to accept connections
Write-Output "[pg-mcp-agent] Waiting for Postgres to accept connections on localhost:5432..."
$maxAttempts = 60
$attempt = 0
while ($attempt -lt $maxAttempts) {
    try {
        $t = Test-NetConnection -ComputerName '127.0.0.1' -Port 5432 -WarningAction SilentlyContinue
        if ($t.TcpTestSucceeded) { break }
    } catch { }
    Start-Sleep -Seconds 1
    $attempt++
}

if ($attempt -ge $maxAttempts) {
    Write-Error "Postgres did not become available within $maxAttempts seconds. Check Docker logs: 'docker compose logs postgres'"
    exit 1
}

Write-Output "[pg-mcp-agent] Postgres is up. Connection string: postgres://pgmcp:pgmcppass@localhost:5432/pgmcp_db"
Write-Output "[pg-mcp-agent] Adminer available at: http://localhost:8080 (user: pgmcp / password: pgmcppass)"

if (-not $NoMock) {
    Write-Output "[pg-mcp-agent] mock_mcp_server will be started as a container (service: mock_mcp_server)."
    Write-Output "[pg-mcp-agent] To view container logs: docker compose logs -f mock_mcp_server"

    # Wait for the mock_mcp_server container to report healthy via its healthcheck
    $maxAttempts = 60
    $attempt = 0
    while ($attempt -lt $maxAttempts) {
        try {
            $cid = docker compose ps -q mock_mcp_server 2>$null
            if ($cid) {
                $health = docker inspect -f "{{.State.Health.Status}}" $cid 2>$null
                if ($health -eq 'healthy') { break }
                if ($health -eq 'unhealthy') { Write-Warning "mock_mcp_server health = unhealthy; check logs."; break }
            }
        } catch { }
        Start-Sleep -Seconds 1
        $attempt++
    }

    if ($attempt -ge $maxAttempts) {
        Write-Warning "mock_mcp_server container did not reach healthy state within $maxAttempts seconds. Check 'docker compose ps' and container logs."
    } else {
        Write-Output "[pg-mcp-agent] mock_mcp_server container is healthy."
    }
}


Write-Output "[pg-mcp-agent] Done."

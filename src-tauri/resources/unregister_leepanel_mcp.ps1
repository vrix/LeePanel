param(
  [Parameter(Mandatory = $true)]
  [string]$LeePanelPath,
  [string]$CodexPath
)

$ErrorActionPreference = 'Stop'
$codex = if ($CodexPath) {
  (Resolve-Path -LiteralPath $CodexPath).Path
} else {
  Get-ChildItem -Path "$env:LOCALAPPDATA\OpenAI\Codex\bin" `
    -Recurse -Filter codex.exe -File -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1 -ExpandProperty FullName
}

if (-not $codex) {
  $codex = Get-Command codex -ErrorAction SilentlyContinue |
    Select-Object -First 1 -ExpandProperty Source
}
if (-not $codex) { exit 0 }

$previousPreference = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
$registeredJson = & $codex mcp get leepanel --json 2>$null
$registeredGetCode = $LASTEXITCODE
$ErrorActionPreference = $previousPreference
if ($registeredGetCode -ne 0 -or -not $registeredJson) { exit 0 }

try {
  $registered = $registeredJson | ConvertFrom-Json
  $actualPath = [IO.Path]::GetFullPath($registered.transport.command)
  $expectedPath = [IO.Path]::GetFullPath($LeePanelPath)
} catch { exit 0 }

if ($actualPath -ne $expectedPath) {
  Write-Host "LeePanel MCP points to another installation; registration was preserved: $actualPath"
  exit 0
}

& $codex mcp remove leepanel | Out-Null
if ($LASTEXITCODE -ne 0) {
  throw 'Failed to remove the LeePanel MCP registration.'
}

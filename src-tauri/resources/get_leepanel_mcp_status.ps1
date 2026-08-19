param(
  [Parameter(Mandatory = $true)]
  [string]$LeePanelPath,
  [string]$CodexPath
)

$ErrorActionPreference = 'Stop'
$leepanel = [IO.Path]::GetFullPath($LeePanelPath)
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

$status = [ordered]@{
  codex_found = [bool]$codex
  codex_path = if ($codex) { $codex } else { '' }
  registered = $false
  current = $false
  registered_path = ''
  version = ''
}

function Get-LeePanelVersion([string]$path) {
  $startInfo = [Diagnostics.ProcessStartInfo]::new($path, '--mcp-version')
  $startInfo.UseShellExecute = $false
  $startInfo.RedirectStandardOutput = $true
  $startInfo.CreateNoWindow = $true
  $process = [Diagnostics.Process]::Start($startInfo)
  $output = $process.StandardOutput.ReadToEnd().Trim()
  $process.WaitForExit()
  if ($process.ExitCode -ne 0) { return '' }
  return $output
}

if ($codex) {
  $previousPreference = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  $registeredJson = & $codex mcp get leepanel --json 2>$null
  $code = $LASTEXITCODE
  $ErrorActionPreference = $previousPreference
  if ($code -eq 0 -and $registeredJson) {
    try {
      $registered = $registeredJson | ConvertFrom-Json
      $registeredPath = [IO.Path]::GetFullPath($registered.transport.command)
      $arguments = @($registered.transport.args)
      $status.registered = $true
      $status.registered_path = $registeredPath
      $status.current = $registeredPath -eq $leepanel -and
        $arguments.Count -eq 1 -and $arguments[0] -eq '--mcp'
    } catch {}
  }
}

try { $status.version = Get-LeePanelVersion $leepanel } catch {}
$status | ConvertTo-Json -Compress

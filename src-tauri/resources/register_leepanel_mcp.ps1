param(
  [Parameter(Mandatory = $true)]
  [string]$LeePanelPath,
  [string]$CodexPath
)

$ErrorActionPreference = 'Stop'
$leepanel = (Resolve-Path -LiteralPath $LeePanelPath).Path

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

if (-not $codex) {
  Write-Host 'Codex CLI was not found. LeePanel will retry registration next time it starts.'
  exit 0
}

function Add-LeePanelMcp([string]$command, $commandArgs, $commandEnv) {
  $arguments = @('mcp', 'add', 'leepanel')
  if ($commandEnv) {
    foreach ($property in $commandEnv.PSObject.Properties) {
      $arguments += @('--env', "$($property.Name)=$($property.Value)")
    }
  }
  $arguments += @('--', $command)
  if ($commandArgs) { $arguments += @($commandArgs) }
  $previousPreference = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  & $codex @arguments | Out-Null
  $code = $LASTEXITCODE
  $ErrorActionPreference = $previousPreference
  return $code
}

function Restore-PreviousMcp($previous) {
  $previousPreference = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  & $codex mcp remove leepanel 2>$null | Out-Null
  $ErrorActionPreference = $previousPreference
  if (-not $previous) { return $true }
  $code = Add-LeePanelMcp `
    $previous.transport.command `
    $previous.transport.args `
    $previous.transport.env
  return $code -eq 0
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

$previous = $null
$previousPreference = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
$previousJson = & $codex mcp get leepanel --json 2>$null
$previousGetCode = $LASTEXITCODE
$ErrorActionPreference = $previousPreference
if ($previousGetCode -eq 0 -and $previousJson) {
  try { $previous = $previousJson | ConvertFrom-Json } catch {}
}

$expectedPath = [IO.Path]::GetFullPath($leepanel)
$alreadyCurrent = $false
if ($previous) {
  try {
    $actualPath = [IO.Path]::GetFullPath($previous.transport.command)
    $actualArgs = @($previous.transport.args)
    $alreadyCurrent = $actualPath -eq $expectedPath -and
      $actualArgs.Count -eq 1 -and $actualArgs[0] -eq '--mcp'
  } catch {}
}

if ($alreadyCurrent) { exit 0 }
if ($previous) {
  $previousPreference = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  & $codex mcp remove leepanel 2>$null | Out-Null
  $ErrorActionPreference = $previousPreference
}
$addCode = Add-LeePanelMcp $leepanel @('--mcp') $null
if ($addCode -ne 0) {
  $restored = Restore-PreviousMcp $previous
  throw "Failed to register LeePanel MCP. Previous registration restored: $restored"
}

$registeredJson = & $codex mcp get leepanel --json 2>$null
$registered = $null
if ($LASTEXITCODE -eq 0 -and $registeredJson) {
  try { $registered = $registeredJson | ConvertFrom-Json } catch {}
}
$actualArgs = @($registered.transport.args)
if (-not $registered -or
    [IO.Path]::GetFullPath($registered.transport.command) -ne $expectedPath -or
    $actualArgs.Count -ne 1 -or $actualArgs[0] -ne '--mcp') {
  $restored = Restore-PreviousMcp $previous
  throw "LeePanel MCP verification failed. Previous registration restored: $restored"
}

$version = Get-LeePanelVersion $leepanel
if (-not $version) {
  $restored = Restore-PreviousMcp $previous
  throw "LeePanel MCP version verification failed. Previous registration restored: $restored"
}

Write-Host "LeePanel MCP $version registered successfully. Restart ChatGPT/Codex to use it."

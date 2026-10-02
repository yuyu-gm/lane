[CmdletBinding()]
param(
    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\lane",
    [string]$GuideDir = "$env:USERPROFILE\.codex",
    [switch]$Apply
)
$ErrorActionPreference = 'Stop'
function Get-LaneSha256([string]$Path) {
    $Algorithm = [Security.Cryptography.SHA256]::Create()
    $Stream = [IO.File]::OpenRead($Path)
    try { return ([BitConverter]::ToString($Algorithm.ComputeHash($Stream))).Replace('-', '').ToLowerInvariant() }
    finally { $Stream.Dispose(); $Algorithm.Dispose() }
}
function Get-LaneAgentsBytes([string]$Path) {
    # Preserve UTF-8/BOM, line endings and all content outside the managed block.
    $Encoding = [Text.UTF8Encoding]::new($false, $true)
    [byte[]]$Original = @()
    if (Test-Path -LiteralPath $Path) { $Original = [IO.File]::ReadAllBytes($Path) }
    $HasBom = $Original.Length -ge 3 -and $Original[0] -eq 0xef -and $Original[1] -eq 0xbb -and $Original[2] -eq 0xbf
    $Offset = if ($HasBom) { 3 } else { 0 }
    $Text = $Encoding.GetString($Original, $Offset, ($Original.Length - $Offset))
    $NewlineMatch = [regex]::Match($Text, '\r\n|\n|\r')
    $Newline = if ($NewlineMatch.Success) { $NewlineMatch.Value } else { "`n" }
    $Start = '<!-- lane-managed-reference:start -->'
    $End = '<!-- lane-managed-reference:end -->'
    $Starts = [regex]::Matches($Text, [regex]::Escape($Start))
    $Ends = [regex]::Matches($Text, [regex]::Escape($End))
    if ($Starts.Count -ne $Ends.Count -or $Starts.Count -gt 1 -or ($Starts.Count -eq 1 -and $Starts[0].Index -ge $Ends[0].Index)) {
        throw 'AGENTS.md has ambiguous Lane markers; preserve it and resolve the markers before installation.'
    }
    $Block = @(
        $Start,
        '## Lane',
        '',
        'Before using Lane for isolated worker work, read and follow [LANE.md](LANE.md) in this same directory.',
        'Lane does not require delegation for ordinary single-agent tasks.',
        $End
    ) -join $Newline
    if ($Starts.Count -eq 1) {
        $After = $Ends[0].Index + $End.Length
        $Text = $Text.Substring(0, $Starts[0].Index) + $Block + $Text.Substring($After)
    } else {
        if ($Text.Length) {
            if (-not $Text.EndsWith($Newline)) { $Text += $Newline }
            if (-not $Text.EndsWith($Newline + $Newline)) { $Text += $Newline }
        }
        $Text += $Block + $Newline
    }
    $Payload = $Encoding.GetBytes($Text)
    if ($HasBom) { $Payload = [byte[]](@(0xef, 0xbb, 0xbf) + $Payload) }
    return ,$Payload
}
$Bundle = Split-Path -Parent $MyInvocation.MyCommand.Path
$HashPath = Join-Path $Bundle 'SHA256SUMS.json'
if (-not (Test-Path -LiteralPath $HashPath)) { throw 'Run install.ps1 from a prepared dist bundle.' }
$Hashes = Get-Content -LiteralPath $HashPath -Raw -Encoding UTF8 | ConvertFrom-Json
foreach ($Name in @('lane.exe', 'LANE.md', 'schema.json', 'install.ps1', 'INSTALL.md', 'LICENSE')) {
    $Expected = $Hashes.PSObject.Properties[$Name].Value
    if (-not $Expected -or (Get-LaneSha256 (Join-Path $Bundle $Name)) -ne $Expected) { throw "Bundle verification failed: $Name" }
}
$SourceExe = Join-Path $Bundle 'lane.exe'
$Version = & $SourceExe --version
if ($LASTEXITCODE -ne 0) { throw 'Bundled executable failed version check' }
$Version | ConvertFrom-Json | Out-Null
$AgentsTarget = [IO.Path]::GetFullPath((Join-Path $GuideDir 'AGENTS.md'))
$AgentsHashBefore = if (Test-Path -LiteralPath $AgentsTarget) { Get-LaneSha256 $AgentsTarget } else { $null }
$AgentsBytes = Get-LaneAgentsBytes $AgentsTarget
$Targets = @(
    @{ Source = $SourceExe; Target = [IO.Path]::GetFullPath((Join-Path $InstallDir 'lane.exe')); Name = 'lane.exe' },
    @{ Source = (Join-Path $Bundle 'LANE.md'); Target = [IO.Path]::GetFullPath((Join-Path $GuideDir 'LANE.md')); Name = 'LANE.md' },
    @{ Source = '<generated Lane reference>'; Target = $AgentsTarget; Name = 'AGENTS.md'; Bytes = $AgentsBytes }
)
foreach ($Item in $Targets) {
    if ($Item.Target.StartsWith($Bundle + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Installation target overlaps bundle source' }
    Write-Host "$($Item.Source) -> $($Item.Target)"
}
if (-not $Apply) { Write-Host 'Preview only. Explicit installation uses -Apply.'; return }
$Backup = Join-Path $Bundle ('backups\' + [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss-fffffff'))
New-Item -ItemType Directory -Path $Backup | Out-Null
$Completed = @()
try {
    foreach ($Item in $Targets) {
        if ($Item.Name -eq 'AGENTS.md') {
            $CurrentHash = if (Test-Path -LiteralPath $Item.Target) { Get-LaneSha256 $Item.Target } else { $null }
            if ($CurrentHash -ne $AgentsHashBefore) { throw 'AGENTS.md changed after preview; retry installation with no other writers.' }
        }
        $Parent = Split-Path -Parent $Item.Target
        New-Item -ItemType Directory -Force -Path $Parent | Out-Null
        $Item.Existed = Test-Path -LiteralPath $Item.Target
        $Item.Backup = Join-Path $Backup $Item.Name
        if ($Item.Existed) { Copy-Item -LiteralPath $Item.Target -Destination $Item.Backup }
        $Temporary = Join-Path $Parent ('.lane-stage-' + [Guid]::NewGuid().ToString('N') + '.tmp')
        try {
            if ($Item.ContainsKey('Bytes')) { [IO.File]::WriteAllBytes($Temporary, $Item.Bytes) }
            else { Copy-Item -LiteralPath $Item.Source -Destination $Temporary }
            if ($Item.Existed) { [IO.File]::Replace($Temporary, $Item.Target, ($Item.Backup + '.atomic')) }
            else { [IO.File]::Move($Temporary, $Item.Target) }
            $Completed += $Item
        } finally { if (Test-Path -LiteralPath $Temporary) { Remove-Item -LiteralPath $Temporary } }
        if ($Item.ContainsKey('Bytes')) {
            $Algorithm = [Security.Cryptography.SHA256]::Create()
            try { $ExpectedHash = ([BitConverter]::ToString($Algorithm.ComputeHash($Item.Bytes))).Replace('-', '').ToLowerInvariant() }
            finally { $Algorithm.Dispose() }
        } else { $ExpectedHash = Get-LaneSha256 $Item.Source }
        if ((Get-LaneSha256 $Item.Target) -ne $ExpectedHash) { throw "Installed file verification failed: $($Item.Target)" }
    }
    & $Targets[0].Target --version
    if ($LASTEXITCODE -ne 0) { throw 'Installed executable version check failed' }
    Write-Host "Applied. Previous files are backed up in $Backup"
} catch {
    $OriginalFailure = $_
    $RollbackErrors = @()
    for ($Index = $Completed.Count - 1; $Index -ge 0; $Index--) {
        $Item = $Completed[$Index]
        try {
            if ($Item.Existed) {
                $Restore = Join-Path (Split-Path -Parent $Item.Target) ('.lane-restore-' + [Guid]::NewGuid().ToString('N') + '.tmp')
                try { Copy-Item -LiteralPath $Item.Backup -Destination $Restore; [IO.File]::Replace($Restore, $Item.Target, ($Item.Backup + '.rollback-replaced')) }
                finally { if (Test-Path -LiteralPath $Restore) { Remove-Item -LiteralPath $Restore } }
            } else { Remove-Item -LiteralPath $Item.Target }
        } catch { $RollbackErrors += $_.Exception.Message }
    }
    if ($RollbackErrors.Count) { throw "Apply failed: $OriginalFailure. Rollback needs inspection: $($RollbackErrors -join '; '). Backups: $Backup" }
    throw "Apply failed and replaced files were rolled back: $OriginalFailure. Backups: $Backup"
}

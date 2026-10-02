[CmdletBinding()]
param([switch]$SkipTests)
$ErrorActionPreference = 'Stop'
function Get-LaneSha256([string]$Path) {
    $Algorithm = [Security.Cryptography.SHA256]::Create()
    $Stream = [IO.File]::OpenRead($Path)
    try { return ([BitConverter]::ToString($Algorithm.ComputeHash($Stream))).Replace('-', '').ToLowerInvariant() }
    finally { $Stream.Dispose(); $Algorithm.Dispose() }
}
$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Split-Path -Parent $ScriptRoot
$Manifest = Join-Path $ProjectRoot 'Cargo.toml'
$Target = Join-Path $ProjectRoot 'target'

& cargo fmt --check --manifest-path $Manifest
if ($LASTEXITCODE -ne 0) { throw 'Rust formatting check failed' }
& cargo clippy --locked --manifest-path $Manifest --target-dir $Target --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { throw 'Rust static checks failed' }
& cargo build --release --locked --manifest-path $Manifest --target-dir $Target
if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed' }
$NativeExe = Join-Path $Target 'release\lane.exe'
if (-not $SkipTests) {
    & cargo test --locked --manifest-path $Manifest --target-dir $Target
    if ($LASTEXITCODE -ne 0) { throw 'Rust tests failed' }
    $PreviousTestExe = $env:LANE_NATIVE_EXE
    try {
        $env:LANE_NATIVE_EXE = $NativeExe
        & python -X utf8 -m unittest discover -s (Join-Path $ProjectRoot 'tests') -v
        if ($LASTEXITCODE -ne 0) { throw 'Executable regression tests failed' }
    } finally { $env:LANE_NATIVE_EXE = $PreviousTestExe }
}
$VersionText = & $NativeExe --version
if ($LASTEXITCODE -ne 0) { throw 'Native version check failed' }
$Version = ($VersionText | ConvertFrom-Json).data.version
$Bundle = Join-Path $ProjectRoot "dist\lane-$Version"
New-Item -ItemType Directory -Force -Path $Bundle | Out-Null
Copy-Item -LiteralPath $NativeExe -Destination (Join-Path $Bundle 'lane.exe') -Force
Copy-Item -LiteralPath (Join-Path $ProjectRoot 'docs\LANE.md') -Destination (Join-Path $Bundle 'LANE.md') -Force
Copy-Item -LiteralPath (Join-Path $ProjectRoot 'LICENSE') -Destination (Join-Path $Bundle 'LICENSE') -Force
Copy-Item -LiteralPath (Join-Path $ScriptRoot 'install.ps1') -Destination (Join-Path $Bundle 'install.ps1') -Force
Copy-Item -LiteralPath (Join-Path $ProjectRoot 'docs\installation.md') -Destination (Join-Path $Bundle 'INSTALL.md') -Force
$Schema = & $NativeExe schema
if ($LASTEXITCODE -ne 0) { throw 'Native schema check failed' }
[IO.File]::WriteAllText((Join-Path $Bundle 'schema.json'), ($Schema + "`n"), [Text.UTF8Encoding]::new($false))
$Hashes = @{}
foreach ($Name in @('lane.exe', 'LANE.md', 'schema.json', 'install.ps1', 'INSTALL.md', 'LICENSE')) {
    $Hashes[$Name] = Get-LaneSha256 (Join-Path $Bundle $Name)
}
[IO.File]::WriteAllText((Join-Path $Bundle 'SHA256SUMS.json'), (($Hashes | ConvertTo-Json) + "`n"), [Text.UTF8Encoding]::new($false))
if ((Get-LaneSha256 (Join-Path $Bundle 'LANE.md')) -ne (Get-LaneSha256 (Join-Path $ProjectRoot 'docs\LANE.md'))) { throw 'Guide copy differs from canonical source' }
if (-not $SkipTests) {
    & python -X utf8 (Join-Path $ProjectRoot 'tests\check_install.py')
    if ($LASTEXITCODE -ne 0) { throw 'Installation checks failed' }
}
Write-Host "Prepared locally: $Bundle"
Write-Host 'No installation, PATH updates or global guide writes performed.'

# Release build: cargo build --release, then the NSIS installer into dist/.
# Usage (from anywhere): pwsh installer/build.ps1
# Honours CARGO_TARGET_DIR; finds makensis via MAKENSIS, PATH, Program Files or Tauri's cached copy.
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$app = Join-Path $root 'app'

$version = (Select-String -Path (Join-Path $app 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value

Push-Location $app
try { cargo build --release; if ($LASTEXITCODE) { throw "cargo build failed" } } finally { Pop-Location }
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $app 'target' }
$exe = Join-Path $target 'release\standbuddy.exe'

$makensis = @(
    $env:MAKENSIS
    (Get-Command makensis -ErrorAction SilentlyContinue).Source
    "${env:ProgramFiles(x86)}\NSIS\makensis.exe"
    "$env:ProgramFiles\NSIS\makensis.exe"
    "$env:LOCALAPPDATA\tauri\NSIS\makensis.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $makensis) { throw "makensis not found (install NSIS or set MAKENSIS)" }

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null
$out = Join-Path $dist "StandBuddy_${version}_x64-setup.exe"
& $makensis /V2 "/DVERSION=$version" "/DEXE=$exe" "/DOUTFILE=$out" (Join-Path $PSScriptRoot 'standbuddy.nsi')
if ($LASTEXITCODE) { throw "makensis failed" }
Write-Output $out

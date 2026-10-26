#Requires -Version 5.1
<#
.SYNOPSIS
  Release build + Inno Setup packaging for the MyPaper Print Agent (Windows).
.EXAMPLE
  .\packaging\build.ps1
  .\packaging\build.ps1 -SkipTests
#>
param(
    [switch]$SkipTests
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
Set-Location $root

$version = (Select-String -Path crates/ui/Cargo.toml -Pattern '^version = "(.*)"').Matches.Groups[1].Value
Write-Host "== mpa $version =="

# Rasterizador PDF: sin esta DLL el análisis de PDF no arranca.
& (Join-Path $root "packaging\fetch-pdfium.ps1")
if ($LASTEXITCODE -ne 0) { exit 1 }

if (-not $SkipTests) {
    cargo test --workspace --locked
    if ($LASTEXITCODE -ne 0) { exit 1 }
    cargo clippy --workspace --all-targets --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

cargo build --release --locked -p mpa-cli -p mpa-ui
if ($LASTEXITCODE -ne 0) { exit 1 }

$iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
if (-not (Test-Path $iscc)) { throw "ISCC.exe not found (choco install innosetup)" }

& $iscc "/DAppVersion=$version" packaging\mpa.iss
if ($LASTEXITCODE -ne 0) { exit 1 }

$setup = Get-ChildItem "Output\mpa-$version-setup.exe" | Select-Object -First 1
Write-Host "installer: $($setup.FullName)"

# Sign + hash for the release manifest (needs packaging/minisign.key).
if (Test-Path packaging\minisign.key) {
    if (Get-Command rsign -ErrorAction SilentlyContinue) {
        & rsign sign $setup.FullName -s packaging\minisign.key -x "$($setup.FullName).minisig" -W
    } else {
        Write-Warning "rsign not found (cargo install rsign2); skipping signature"
    }
    $hash = (Get-FileHash $setup.FullName -Algorithm SHA256).Hash.ToLower()
    [pscustomobject]@{
        version   = $version
        url       = "https://github.com/<org>/mypaper-print-agent/releases/download/v$version/mpa-$version-setup.exe"
        sha256    = $hash
        signature = $null
        notes     = $null
    } | ConvertTo-Json | Out-File "Output\latest.json" -Encoding utf8
    Write-Host "manifest: Output\latest.json (paste signature + final URL before publishing)"
} else {
    Write-Warning "packaging\minisign.key missing; installer unsigned"
}

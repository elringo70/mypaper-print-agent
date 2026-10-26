# Fetches the pdfium shared library used by the analyzer.

<#
.SYNOPSIS
  Downloads pdfium (BSD-3-Clause) into vendor/pdfium for the release build.

.DESCRIPTION
  The analyzer rasterizes PDFs with pdfium instead of Ghostscript: same
  measurements, a redistributable licence, and one pinned version so the
  numbers do not drift with whatever the customer has installed.

  The version must match the `pdfium_XXXX` feature pdfium-render resolves
  (see Cargo.lock); a mismatch fails at load time with a missing-symbol error.

.EXAMPLE
  .\packaging\fetch-pdfium.ps1
#>
param(
    [string]$Version = "7881"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
$dest = Join-Path $root "vendor\pdfium"
$dll = Join-Path $dest "pdfium.dll"

if (Test-Path $dll) {
    Write-Host "pdfium already present: $dll"
    exit 0
}

New-Item -ItemType Directory -Force -Path $dest | Out-Null
$url = "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/$Version/pdfium-win-x64.tgz"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "pdfium-win-x64-$Version.tgz"

Write-Host "downloading pdfium $Version"
Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing

# Windows 10+ ships bsdtar, which reads .tgz natively.
tar -xzf $tmp -C $dest
if (-not (Test-Path $dll)) {
    $found = Get-ChildItem -Path $dest -Recurse -Filter "pdfium.dll" | Select-Object -First 1
    if (-not $found) { throw "pdfium.dll not found in the archive" }
    Copy-Item $found.FullName $dll
}
Remove-Item $tmp -Force

Write-Host "pdfium.dll -> $dll"
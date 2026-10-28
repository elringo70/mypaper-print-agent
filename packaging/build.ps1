#Requires -Version 5.1
<#
.SYNOPSIS
  Release build + Inno Setup packaging for the MyPaper Print Agent (Windows).

.DESCRIPTION
  Genera un instalador "siguiente, siguiente, siguiente" con todo lo que el
  agente necesita para arrancar en una máquina limpia:

    mpa.exe            CLI (pair, doctor, entitlement, autostart)
    mpa-desktop.exe    ventana Tauri con el frontend incrustado
    pdfium.dll         rasterizador PDF (BSD-3), sin Ghostscript
    WebView2           runtime, sólo si el equipo no lo trae ya

  Requiere: Rust MSVC, Node 20+, Inno Setup 6. La primera ejecución descarga
  pdfium y, salvo -SkipWebView2, el instalador offline de WebView2 (~127 MB).

.EXAMPLE
  .\packaging\build.ps1
  .\packaging\build.ps1 -SkipTests -SkipWebView2
#>
param(
    [switch]$SkipTests,
    [switch]$SkipWebView2
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
Set-Location $root

$version = (Select-String -Path crates/desktop/Cargo.toml -Pattern '^version = "(.+)"').Matches.Groups[1].Value
Write-Host "== mpa $version =="

# --- Dependencias -----------------------------------------------------------------
& (Join-Path $root "packaging\fetch-pdfium.ps1")
if ($LASTEXITCODE -ne 0) { exit 1 }

if (-not $SkipWebView2) {
    & (Join-Path $root "packaging\fetch-webview2.ps1")
    if ($LASTEXITCODE -ne 0) { exit 1 }
} else {
    Write-Warning "WebView2 offline omitido: el instalador avisará si el equipo no lo tiene."
}

# --- Frontend ---------------------------------------------------------------------
# El binario incrusta el bundle: si esto se salta, el exe sirve la UI vieja.
$ui = Join-Path $root "crates\desktop\ui"
Push-Location $ui
try {
    if (Test-Path "package-lock.json") { npm ci } else { npm install }
    if ($LASTEXITCODE -ne 0) { exit 1 }
    npm run build
    if ($LASTEXITCODE -ne 0) { exit 1 }
} finally {
    Pop-Location
}
if (-not (Test-Path (Join-Path $ui "dist\index.html"))) {
    throw "el frontend no se compiló (falta dist\index.html)"
}

# --- Rust -------------------------------------------------------------------------
# mpa-ui is retired: mpa-desktop replaced it and the installer deletes the old
# mpa-ui.exe. The crate no longer compiles against mpa-agent and nothing depends
# on it, so it is excluded here the same way docs/plan-agente-v2.md excludes it.
#
# mpa-desktop's own test binaries carry its requireAdministrator manifest, so
# running them from a non-elevated shell fails with "os error 740" (the same
# reason the docs exclude it on Linux). Clippy still compiles every target,
# including that one; only execution is skipped here.
if (-not $SkipTests) {
    cargo test --workspace --exclude mpa-ui --exclude mpa-desktop --locked
    if ($LASTEXITCODE -ne 0) { exit 1 }
    cargo clippy --workspace --exclude mpa-ui --all-targets --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) { exit 1 }
}

# custom-protocol va por defecto: el binario embebe el frontend.
cargo build --release --locked -p mpa-cli -p mpa-desktop
if ($LASTEXITCODE -ne 0) { exit 1 }

# --- Instalador -------------------------------------------------------------------
# Inno puede estar por máquina (Program Files) o por usuario (winget sin UAC).
$iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
if (-not (Test-Path $iscc)) {
    $iscc = Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe"
}
if (-not (Test-Path $iscc)) {
    throw "ISCC.exe no encontrado. Instala Inno Setup 6: winget install -e --id JRSoftware.InnoSetup"
}

# cargo escribe en target\release, salvo cuando se compiló con --target.
$releaseDir = Join-Path $root "target\x86_64-pc-windows-msvc\release"
if (-not (Test-Path (Join-Path $releaseDir "mpa-desktop.exe"))) {
    $releaseDir = Join-Path $root "target\release"
}
foreach ($exe in @("mpa.exe", "mpa-desktop.exe")) {
    if (-not (Test-Path (Join-Path $releaseDir $exe))) { throw "falta $exe en $releaseDir" }
}

& $iscc "/DMyAppVersion=$version" "/DTargetDir=$releaseDir" packaging\mpa.iss
if ($LASTEXITCODE -ne 0) { exit 1 }

$setup = Get-ChildItem "Output\mpa-$version-setup.exe" | Select-Object -First 1
Write-Host "installer: $($setup.FullName)"

# --- Firma y manifiesto -----------------------------------------------------------
if (Test-Path packaging\minisign.key) {
    if (Get-Command rsign -ErrorAction SilentlyContinue) {
        & rsign sign $setup.FullName -s packaging\minisign.key -x "$($setup.FullName).minisig" -W
    } else {
        Write-Warning "rsign no encontrado (cargo install rsign2); instalador sin firma"
    }
    $hash = (Get-FileHash $setup.FullName -Algorithm SHA256).Hash.ToLower()
    [pscustomobject]@{
        version   = $version
        url       = "https://github.com/<org>/mypaper-print-agent/releases/download/v$version/mpa-$version-setup.exe"
        sha256    = $hash
        signature = $null
        notes     = $null
    } | ConvertTo-Json | Out-File "Output\latest.json" -Encoding utf8
    Write-Host "manifest: Output\latest.json (pega firma y URL final antes de publicar)"
} else {
    Write-Warning "packaging\minisign.key ausente; instalador sin firmar"
}
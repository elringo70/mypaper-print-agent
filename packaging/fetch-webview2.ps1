#Requires -Version 5.1
<#
.SYNOPSIS
  Descarga el instalador offline de WebView2 Runtime (x64) en vendor\webview2.

.DESCRIPTION
  La ventana es un webview: sin WebView2 no abre. El instalador offline pesa
  ~127 MB y hace que la instalación funcione sin internet, que es el caso real
  de una papelería con la red caída. Inno lo embebe sólo si el archivo existe;
  si no, el setup se genera y la comprobación avisa.

  Es redistribuible por Microsoft y no requiere licencia adicional.

.EXAMPLE
  .\packaging\fetch-webview2.ps1
#>
param(
    [string]$Url = "https://go.microsoft.com/fwlink/?linkid=2124701"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $PSCommandPath)
$dest = Join-Path $root "vendor\webview2"
$target = Join-Path $dest "MicrosoftEdgeWebview2RuntimeInstallerX64.exe"

if (Test-Path $target) {
    Write-Host "WebView2 offline ya presente: $target"
    exit 0
}

New-Item -ItemType Directory -Force -Path $dest | Out-Null
Write-Host "descargando WebView2 offline (~127 MB)"
Invoke-WebRequest -Uri $Url -OutFile $target -UseBasicParsing

$size = (Get-Item $target).Length
if ($size -lt 50MB) {
    Remove-Item $target -Force
    throw "descarga sospechosa ($size bytes); ¿cambió el enlace de Microsoft?"
}
Write-Host "WebView2 offline -> $target ($([math]::Round($size/1MB)) MB)"
#Requires -Version 5.1
<#
.SYNOPSIS
  Post-install wiring: printer roles, the MyPaper capture queue, and a check that
  it all answers.

.DESCRIPTION
  The installer runs this. Everything here is idempotent: running it twice must
  not create a second queue or lose the roles.

  Order matters. The queue is added by asking Windows to create an IPP port, and
  Windows queries the endpoint while creating it — so the agent has to be up and
  answering first, or the install ends with "no se encontró el dispositivo".

.PARAMETER Bw
  Printer that prints black and white.
.PARAMETER Color
  Printer that prints in colour.
#>
param(
    [string]$Bw = '',
    [string]$Color = '',
    [int]$Port = 8631,
    [int]$TimeoutSeconds = 30
)

$ErrorActionPreference = 'Stop'
$app = Split-Path -Parent $PSCommandPath

# Installed layout: the script sits next to the binaries. Development check: it
# lives in packaging\ and the binary is under target\. Either way, fail loudly
# rather than half-configuring a machine.
function Resolve-Binary([string]$name) {
    $candidates = @(
        (Join-Path $app $name),
        (Join-Path (Split-Path -Parent $app) $name),
        (Join-Path (Split-Path -Parent $app) "target\debug\$name"),
        (Join-Path (Split-Path -Parent $app) "target\release\$name")
    )
    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) { return $candidate }
    }
    Write-Host "MyPaper: no encuentro $name"
    exit 5
}

$mpa = Resolve-Binary 'mpa.exe'
$desktop = Resolve-Binary 'mpa-desktop.exe'
$queue = 'MyPaper'

function Say($text) { Write-Host "MyPaper: $text" }

# --- 0. Precondiciones -------------------------------------------------------------
# El módulo se vende como análisis de color: sin una impresora de cada rol no hay
# nada que medir, y dejar el agente instalado a medias sólo genera trabajos que
# nadie puede cobrar.
$installed = @(Get-Printer -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name)
if ($installed.Count -lt 2) {
    Say "se necesitan al menos dos impresoras instaladas (una B/N y una de color); hay $($installed.Count)"
    exit 6
}
if (-not $Bw -or -not $Color) {
    Say 'faltan los roles: elige la impresora de blanco y negro y la de color'
    exit 7
}
if ($Bw -eq $Color) {
    Say 'la misma impresora no puede ser la de blanco y negro y la de color'
    exit 8
}
foreach ($role in @($Bw, $Color)) {
    if ($installed -notcontains $role) {
        Say "'$role' no está instalada en este equipo"
        exit 9
    }
}

# --- 1. Roles ---------------------------------------------------------------------
if ($Bw -or $Color) {
    $cliArgs = @('printers', 'set')
    if ($Bw) { $cliArgs += @('--bw', $Bw) }
    if ($Color) { $cliArgs += @('--color', $Color) }
    & $mpa @cliArgs | Out-Host
    if ($LASTEXITCODE -ne 0) { Say "no se pudieron guardar los roles"; exit 3 }
    Say "roles: B/N = $Bw · Color = $Color"
}

# --- 2. The agent must be answering before the queue is created --------------------
$running = Get-Process -Name 'mpa-desktop' -ErrorAction SilentlyContinue
if (-not $running) {
    Say 'iniciando el agente'
    Start-Process -FilePath $desktop -WindowStyle Minimized | Out-Null
}

$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
$answering = $false
while ((Get-Date) -lt $deadline) {
    try {
        $client = New-Object Net.Sockets.TcpClient
        $client.Connect('127.0.0.1', $Port)
        $client.Close()
        $answering = $true
        break
    } catch {
        Start-Sleep -Milliseconds 400
    }
}
if (-not $answering) {
    Say "el agente no responde en el puerto $Port; la cola no se creó"
    exit 2
}
Say "agente respondiendo en 127.0.0.1:$Port"

# --- 3. The capture queue ---------------------------------------------------------
$existing = Get-Printer -Name $queue -ErrorAction SilentlyContinue
if ($existing) {
    Say "la cola $queue ya existe (puerto $($existing.PortName))"
} else {
    Add-Printer -IppURL "http://127.0.0.1:$Port/ipp/print" -Name $queue
    Say "cola $queue creada"
}

# --- 4. Prove it ------------------------------------------------------------------
$created = Get-Printer -Name $queue -ErrorAction SilentlyContinue
if (-not $created) {
    Say "la cola $queue no quedó registrada"
    exit 4
}
Say "listo: imprime a '$queue' desde cualquier programa y el agente lo medirá"
exit 0

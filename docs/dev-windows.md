# Dev loop en Windows

El agente sólo compila y corre en Windows: `crates/spool` usa WinSpool por FFI,
la ventana necesita WebView2 y el proceso pide elevación para leer
`C:\Windows\System32\spool\PRINTERS`. WSL sirve para editar y para git, no para
compilar este repo.

## Requisitos (verificados en la máquina de desarrollo)

| Pieza | Valor encontrado | Nota |
| --- | --- | --- |
| rustc / cargo | 1.99.1, host `x86_64-pc-windows-msvc` | único toolchain instalado |
| Visual Studio Build Tools | 2022, componente `VC.Tools.x86.x64` | necesario para `link.exe` |
| WebView2 Runtime | 156.0.4314.50 | ya presente; el instalador lo trae offline de respaldo |
| Node / npm | 24.12.0 / 11.21.1 | npm, no bun: `ui/package-lock.json` es de npm |

## Loop

```powershell
cd C:\mypaper-print-agent\crates\desktop
npm run tauri            # tauri dev: HMR de Svelte + reinicio al cambiar Rust
npm run tauri:dev:nowatch
```

`npm run tauri` ejecuta `cargo run --no-default-features`: la feature
`custom-protocol` (que incrusta `ui/dist` en el binario) queda apagada, así que
la ventana carga `http://127.0.0.1:1420` y ves los cambios sin recompilar.

La consola **debe** estar elevada. Si no, el binario muere con `os error 740`
porque `mpa-desktop.manifest` declara `requireAdministrator`.

## Un workflow de git por sistema de archivos

Nunca uses el mismo working tree desde Windows y desde WSL. El repo vive en
`C:\mypaper-print-agent` y se opera con el git de Windows (`git.exe`). Si editas
desde WSL con `/mnt/c/mypaper-print-agent`, **no** ejecutes comandos de git ahí:
el índice y los bits de permiso se pelean (`core.fileMode`, CRLF).

Configuración ya aplicada en el clon de Windows:

```powershell
git config core.autocrlf false
git config core.fileMode false
```

`.gitattributes` fuerza `* text=auto eol=lf`, así que ambos lados ven los mismos
bytes.

## Tests del análisis, en caliente

```powershell
cargo install cargo-watch --locked
cargo watch -x "test -p mpa-analyzer" -x "test -p mpa-domain"
```

`crates/analyzer` concentra la lógica pura de color (cobertura CMYK, `inkcov`,
normalización de PDF) y no depende de Tauri; por eso el loop de tests no necesita
la ventana. Las capturas reales de spool van en
`crates/analyzer/tests/fixtures/spool/` (ver su README).

## Hilos y eventos

El estado ya está diseñado para no congelar la ventana:

- Todos los comandos son `#[tauri::command(async)]`: corren en el pool del
  runtime, no en el hilo del WebView.
- El análisis y la impresión viven en hilos propios (`spawn_ticket_refresh`,
  `spawn_outbox_drain`, el runtime del agente).
- La ventana no recibe snapshots por evento: sondea
  `version()`, un contador, y sólo pide el snapshot cuando cambió. Un evento por
  página sería más caro que el problema que resuelve.

Regla: nada de resultados grandes por IPC. Miniaturas como `data:` sólo para la
página visible; el resto por ruta.

## Defender y antivirus

Cada `link.exe` de MSVC produce un `.pdb`/`.exe` nuevo y Defender lo escanea:
son minutos por rebuild. Excluir (PowerShell **elevado**, lo corres tú):

```powershell
Add-MpPreference -ExclusionPath "C:\mypaper-print-agent\target"
Add-MpPreference -ExclusionPath "$env:USERPROFILE\.cargo"
Add-MpPreference -ExclusionPath "$env:USERPROFILE\.rustup"
```

Revertir:

```powershell
Remove-MpPreference -ExclusionPath "C:\mypaper-print-agent\target"
```

Costo: un binario malicioso en esas rutas no se escanea. Es caché de compilación
propia, no descargas arbitrarias.

## sccache: cuándo sí

`sccache` cachea compilaciones de Rust entre `target/` distintos. Aquí aplica
poco: sólo recompilas tus 11 crates, no las ~500 dependencias. Sí conviene si:

- compilas debug y release del mismo commit seguido, o
- dos checkouts del repo viven en la misma máquina (hoy hay dos: `C:\mypaper-print-agent`
  y `C:\mpa-build\mypaper-print-agent`).

```powershell
cargo install sccache --locked
# .cargo\config.toml del repo
[build]
rustc-wrapper = "sccache"
```

Medirlo antes de adoptarlo: `sccache --show-stats`. Si el hit rate es bajo,
sobra complejidad.

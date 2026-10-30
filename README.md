# MyPaper Print Agent

Agente local de impresión para papelerías. Vive como **proyecto aparte de la
webapp** (sidecar): se compila para Windows, corre en la PC del negocio junto al
spooler y se comunica con MyPaper por HTTP (`/api/agent/*`).

## Estado: **alpha** (fase 1)

Sirve para probar el ciclo completo en una máquina real. **No está listo para
producción**: ver *Pendientes*.

Funciona y está probado en Windows:

- **Retención de trabajos**: intercepta cada trabajo del spooler y lo deja en
  pausa antes de imprimir (pause / resume / cancel).
- **Análisis por página**: cobertura CMYK + miniatura, normalizando PCL / EMF /
  XPS / PostScript a PDF (Ghostscript + GhostPCL).
- **UI (gpui)**: lista de trabajos, lista de páginas con casillas, una sola
  ventana de previsualización de la página seleccionada, destino (ticket o
  nuevo borrador) y `Completar e imprimir` / `Cancelar`.
- **Recuperación**: un trabajo retenido antes de reiniciar el agente se vuelve a
  adoptar y analizar al arrancar (el spooler no lo re-notifica).
- **Enlace con MyPaper**: emparejamiento por código, tabulador sincronizado y
  **envío automático del borrador al completar** (draft → ticket → líneas).
- **Diagnóstico**: `mpa doctor` (spooler, herramientas, enlace, niveles, cola) y
  `mpa push-test` (envía un análisis de muestra sin imprimir nada).
- **Auditoría local**: `audit.jsonl` con quién retuvo/completó qué y `mpa.log`.

Pendientes (para las siguientes fases):

- Reintento del análisis cuando el spool llega vacío: el hold bloquea a la app
  que imprime, y una impresión lenta puede analizarse parcial o vacía.
- Seguridad del payload: firma/rotación de claves del agente y cifrado en
  tránsito más allá del Bearer actual (sha256 en BD).
- Instalador firmado (`packaging/mpa.iss` sin compilar) y auto-actualización.
- Tests de UI en Windows: 1 fallo preexistente (fixture que toca el spooler real).
- Cobertura de casos de impresoras de red que renderizan en el servidor.

## Piezas

```
crates/domain     tipos compartidos (cobertura, niveles, estados)
crates/spool      acceso al spooler (Windows + MockSpool para tests)
crates/analyzer   captura del spool, normalización a PDF, inkcov, miniaturas
crates/agent      núcleo: candado, máquina de estados, outbox, runtime
crates/sync       cliente HTTP del contrato /api/agent/* + pairing
crates/ui         ventana gpui + bandeja del sistema
crates/cli        mpa.exe (doctor, pair, watch, analyze, push-test, autostart…)
crates/update     canal de actualización (manifiesto + firma minisign)
third-party/xattr crate parcheado (Cargo.toml [patch.crates-io])
packaging         mpa.iss (Inno Setup) + build.ps1 + minisign.pub
```

## Desarrollar (Windows, ciclo rápido)

No compiles el instalador para revisar un cambio de UI. El loop de desarrollo es
`tauri dev`: compila `mpa-desktop` en **debug**, levanta Vite con HMR y **no**
genera instalador.

```powershell
# Terminal elevada (el manifiesto pide requireAdministrator).
cd C:\mypaper-print-agent

# Una sola vez: CLI de Tauri (shell) + dependencias del frontend (ui/).
npm --prefix crates\desktop install
npm --prefix crates\desktop\ui install

# Loop de desarrollo. Ctrl+C para salir.
cd crates\desktop
npm run tauri
```

El CLI de Tauri se ejecuta **desde `crates\desktop`**: ahí viven `tauri.conf.json`
y `package.json`. Desde `ui/` falla con *"Couldn't recognize the current folder as
a Tauri project"*.

- `npm run tauri` / `npm run tauri:dev` → `tauri dev`.
- `npm run tauri:dev:nowatch` desactiva el reinicio automático por cambios en Rust.
- `cargo tauri dev` es equivalente si instalaste el CLI de cargo: `cargo install tauri-cli --locked`.
- Cambios en `.svelte`/`.ts`/CSS: HMR instantáneo, **cero** compilación Rust.
- Cambios en `crates/desktop/src`: recompila ese crate y reinicia la ventana (segundos).
- Cambios en otros crates (`agent`, `spool`, `sync`): el watcher no los cubre; reinicia con `Ctrl+C` + `npx tauri dev`.
- El `target/` se mantiene caliente: **nunca** corras `cargo clean` para ahorrar tiempo, es lo contrario.
- Excluir `target\` de Windows Defender ahorra minutos de escaneo por link:
  `Add-MpPreference -ExclusionPath "C:\mypaper-print-agent\target"`.
- Sólo el frontend, sin app nativa: `cd crates\desktop\ui; npm run dev` y abre
  `http://127.0.0.1:1420` en el navegador. Los `invoke` fallan ahí: sirve para maquetar.

El CSP de desarrollo vive en `app.security.devCsp` (`crates/desktop/tauri.conf.json`)
y permite el WebSocket de HMR; el CSP de producción sigue sin `unsafe-inline`.

## Compilar y ejecutar (Windows)

```powershell
# MSVC; release + instalador Inno Setup (tarda minutos, no es el loop de dev)
.\packaging\build.ps1
.\packaging\build.ps1 -SkipTests -SkipWebView2
```

- Requiere **Ghostscript 10.08** y **GhostPCL** (`gpcl6win64.exe`) en
  `C:\Program Files\gs\gs10.08.0\bin` (sobreescribibles con `MPA_GHOSTSCRIPT` /
  `MPA_GPCL`).
- `mpa-ui.exe` necesita **elevación**: `C:\Windows\System32\spool\PRINTERS` solo
  abre para Administradores (manifiesto `requireAdministrator`).
- Config y estado: `%APPDATA%\mypaper-print-agent\` → `config.toml`,
  `agent-state.json`, `agent.json` (clave del agente), `mpa.log`, `audit.jsonl`.

## Enlazar con MyPaper

1. En la web, como administrador de un workspace **PREMIUM**:
   `/{slug}/print-tiers` → *Equipos enlazados* → generar código.
2. En la PC:
   ```bat
   mpa pair <CÓDIGO> --pc-name PC-PAPELERIA --server https://tu-dominio
   mpa tiers sync
   mpa doctor
   ```
3. Prueba del enlace sin gastar papel:
   ```bat
   mpa push-test              :: usa el servidor y la clave emparejados
   ```

## Contrato con MyPaper (Fase 0)

| Método | Ruta | Para qué |
|---|---|---|
| POST | `/api/agent/jobs` | Guarda el borrador del análisis (idempotente por `idempotencyKey`) |
| GET | `/api/agent/jobs` | Devuelve los borradores recientes |
| POST | `/api/agent/jobs/{id}/materialize` | Inyecta el borrador en su ticket como líneas |

- Autenticación: `Authorization: Bearer <agent_key>` (se guarda su sha256).
- El servidor es **autoritativo**: clasifica la cobertura con `print_tiers` y
  pone los precios desde el catálogo; el agente **no** envía importes.
- La feature `print:agent` exige plan PREMIUM + suscripción activa.

## Pruebas

```bash
# host (Linux/macOS): la UI necesita libs X11, se excluye
cargo test --workspace --exclude mpa-ui
# Windows (MSVC)
cargo test --release --workspace
```

- `crates/agent/tests/lock_discipline.rs`: el candado del núcleo nunca se
  sostiene durante IO del spooler (la UI lo toma cada 500 ms).
- `crates/agent/tests/held_job_recovery.rs`: un trabajo retenido sobrevive a un
  reinicio del agente.
- `crates/analyzer`: normalización PCL/EMF/XPS y parser de `inkcov`.

## Privacidad y llaves

- **Nunca** commitear `packaging/minisign.key` (está en `.gitignore`): respáldala
  fuera del repo y súbela a los secretos del workflow de release.
- `%APPDATA%\mypaper-print-agent\agent.json` contiene la clave del agente: no
  copiarla a issues ni a capturas.

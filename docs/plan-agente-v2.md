# Plan de evolución — MyPaper Print Agent v2

Estado: propuesta para aprobación. Nada implementado todavía.
Fecha: 2026-10-25. Repo: ~/documents/proyects/mypaper-print-agent

## Decisiones cerradas (del dueño)

1. El core se queda en **Rust**. No se migra a Go.
2. La UI pasa a **Tauri v2 + Svelte**; el proceso "backend" sigue en Rust.
3. Solo dependencias con **licencia libre o gratuita sin costo**.
4. Instalador Windows "next, next, next" que **verifique e instale** todo lo necesario.
5. El REA de CyberPlanet es insumo, no plantilla: se **adapta** a MyPaper.
6. La cobertura que factura MyPaper es **porcentaje de área**.
7. `GRISCOMOCOLOR` se queda como hoy (grises cuentan como color).
8. Librerías adicionales: licencias libres o gratuitas.

## Hallazgo que cambia el plan: Ghostscript no puede quedarse

El analizador actual exige Ghostscript para **todo** PDF/PS (`crates/analyzer/src/gs.rs`,
`crates/analyzer/src/convert.rs`). Ghostscript es AGPL-3.0 y Artifex es explícito:
distribuirlo dentro de, o junto a, una aplicación propietaria exige **licencia comercial
de pago** — "you may not use Ghostscript in any software that you distribute, on pain of
legal action". Ni siquiera invocarlo como proceso separado cambia el análisis de Artifex:
"your application includes one or more functions that use some or all of AGPL or Ghostscript"
cae dentro de su criterio de obra derivada.

Consecuencia: hoy el producto tiene una dependencia que **no cumple** la regla 3 y que además
es el prerrequisito más frágil del instalador. Se sustituye por **pdfium** (BSD-3-Clause,
binario redistribuible) vía el crate `pdfium-render` (MIT/Apache-2.0), y por el playback GDI
que ya existe para EMF. Esto resuelve tres problemas de un golpe: licencia, instalador y
determinismo (pdfium fija la versión del rasterizador, Ghostscript no).

Costo: se pierde soporte PCL (GhostPCL era el único camino libre y venía del mismo paquete
AGPL). Ver decisión D2 al final.

## Fase 0 — Fundaciones (2-3 días)

Objetivo: dejar el terreno firme antes de tocar código de producto.

- `docs/adr/0001-rust-core-tauri-ui.md` — decisión y alcance.
- `docs/adr/0002-license-policy.md` — allowlist: MIT, Apache-2.0, BSD-2/3, ISC, MPL-2.0,
  Zlib, Unicode-3.0. Denylist: GPL, AGPL, LGPL (salvo enlace dinámico justificado), SSPL,
  licencias "source-available" y comerciales. Añadir `cargo-deny` al CI con esa allowlist.
- `docs/adr/0003-drop-ghostscript.md` — el análisis de arriba, con las citas de Artifex.
- Inventario de dependencias actual → objetivo, marcando qué se va (gpui, tray-icon, muda,
  gs, ghostpcl) y qué entra (tauri 2, pdfium-render, ed25519-dalek, rustls, sha2).
- CI: `cargo deny check`, `cargo nextest`, `cargo clippy -D warnings`. Sustituir
  `cargo test` por nextest (más rápido y con salida por test).
- Presupuesto de tiempo: `cargo check` incremental hoy son 13.9 s (medido, sin UI).
  Objetivo: mantener el ciclo de cambio del core por debajo de 20 s.

Cierre: tres ADR aprobados, `cargo deny` verde, CI sin `cargo test` legacy.

## Fase 1 — Analizador v2: medición por área (1.5-2 semanas)

Es el corazón del feature. Sustituye el estimador doble que hay hoy (Ghostscript `inkcov`
para PDF, separación RGB→CMYK naive para imágenes) por **una sola ruta**: documento →
raster 150 dpi → conteo por píxel.

Trabajo:

- Nuevo módulo `mpa-analyzer::classify` con la ruta única, en este orden de importancia:
  - **EMF → playback GDI existente (`emf.rs`) → bitmap.** Es el caso mayoritario: toda
    impresión desde una aplicación Windows, local o de red, llega así cuando la cola usa
    EMF spooling.
  - PDF → `pdfium-render` → bitmap RGBA 150 dpi (cuando la aplicación o el usuario imprime
    un PDF, o se usa la impresora virtual de MyPaper).
  - Imagen (PNG/JPEG/BMP/TIFF) → crate `image` (ya está).
  - **RAW (PCL/PostScript) → no analizable.** Se detecta, se marca el trabajo como
    "sin análisis" y `doctor` señala esa cola como mal configurada, en vez de fallar.
- **EMF spooling garantizado.** El agente enumera las colas y verifica que el print
  processor sea `winprint` con datatype EMF antes de prometer análisis. Si una cola está en
  RAW, no se toca a espaldas del dueño: se reporta y se ofrece corregirlo desde la UI
  (requiere el `requireAdministrator` que ya tenemos). El cambio es reversible y se
  registra para el desinstalador.
- Métricas por página, con los nombres del REA para poder comparar 1:1:
  `coverage_nonwhite` (área con tinta), `coverage_c/m/y/k`, `dark_fraction`,
  `color_pixel_frac`, `gris_pixel_frac`, `pure_black_frac`, `mean_chroma`, `area_cm2`.
- Reglas fijas y versionadas (constantes del binario, no configurables por el operador):
  canal < 248/255 cuenta como tinta; croma > 0.04 = píxel color; gris = croma ≤ 0.008;
  tinta de canal > 0.002; clase: `blanco` si nonwhite ≤ 0.4% sin color, `mono` si
  oscuros ≥ 50% de los píxeles con tinta, `gris` si hay tinta sin color, `color` si
  max(C,M,Y) > 0.002.
- `Coverage` del dominio gana `area_pct` y `class`; el campo `coverage` por canal se
  conserva para no romper nada.
- Test dorado: copiar los 30 fixtures (`CP-001…CP-024`) y `render_truth.json` del paquete
  de REA a `crates/analyzer/tests/fixtures/` (copia física, no symlink, con sus sha256).
  Criterio: 35 páginas, diferencia ≤ 0.05 pp por canal y clase idéntica en las 35.
- Determinismo: mismo binario + misma entrada ⇒ mismo JSON byte a byte, en 5 corridas.
  El JSON lleva `engine.version`, `rasterizer`, `dpi` y `thresholds`.
- Rendimiento: 35 páginas A4 a 150 dpi en < 25 s a un hilo en la PC objetivo; si no se
  cumple, 120 dpi configurable y el umbral se documenta.
- `inkcov` deja de ser fuente de precio. Si se quiere conservar, solo como test cruzado
  opcional en desarrollo.

Cierre: el test dorado pasa, el benchmark se cumple, no queda ninguna ruta que dependa
de Ghostscript para facturar.

## Fase 2 — Precio por área y contrato con el POS (1 semana)

Objetivo: que "porcentaje de área" sea una métrica de primera clase en ambos lados, sin
romper el modelo de dos conjuntos que ya existe.

Rust (`crates/domain`):
- `PrintMetric::Area` nuevo, con `metric_coverage` devolviendo `coverage_nonwhite`.
- `PrintPricing` conserva `intensityThreshold`, `greysAsColor`, `blankPageFree`,
  `paperSurcharge`, `volumeDiscounts` tal cual. `greysAsColor` se queda en `true`
  (regla 7: como hoy).
- Tests de paridad: los mismos casos que `PrintTierService.test.ts`, tabla de vectores
  compartida en JSON para que ambos lados lean el mismo archivo.

POS (`src/lib/services/PrintTierService.ts` y `printAgent.schema.ts`):
- `AREA` en `printMetricSchema` y en `metricCoverage`.
- Default del workspace nuevo sigue siendo `BW=K_ONLY`, `COLOR=SUM_CMYK`; el **preset
  CyberPlanet** cambia `COLOR` a `AREA`.
- Preset "CyberPlanet 6.5" como seed configurable (no hardcode del dump):
  6 tramos 0-10 / 10-20 / 20-40 / 40-55 / 55-70 / 70-100 % → $3 / $4 / $6 / $8 / $10 / $12,
  ByN fijo $1.50, `greysAsColor=true`. Editable desde `print-tiers`, como cualquier tarifa.
- Campos nuevos **opcionales** en `printJobPage`: `class`, `coverageNonwhite`,
  `colorPixelFrac`, `grisPixelFrac`, `engineVersion`. Si no llegan, el POS sigue
  calculando con `c/m/y/k` exactamente como hoy.
- El agente nunca envía importes. El POS sigue siendo autoritativo (ya es así; se mantiene).

Cierre: una cotización del mismo trabajo da el mismo precio calculado en Rust y en el POS
para los 30 fixtures, y el POS acepta payloads viejos sin cambios.

## Fase 3 — Binding criptográfico agente ↔ MyPaper (1 semana)

Objetivo: que el agente no sirva sin la webapp, y que Premium no se pueda saltar desde el
cliente. La respuesta correcta no es ofuscar el binario: es que **el valor viva en el
servidor** y que el servidor no pueda ser engañado.

Diseño propuesto:

1. **Identidad de dispositivo.** Al emparejar, el agente genera un par **Ed25519**
   (`ed25519-dalek`, BSD-3) y guarda la privada en `agent.json` con ACL de usuario.
   Envía la pública en `POST /api/agent/pair`, el servidor la guarda en
   `print_agents.public_key` y devuelve `agent_id` + clave de agente.
2. **Firma por petición.** Cada llamada a `/api/agent/*` lleva
   `X-MPA-Key-Id`, `X-MPA-Ts`, `X-MPA-Nonce` y `X-MPA-Sig` =
   Ed25519(sha256(method | path | ts | nonce | sha256(body))).
3. **Verificación en servidor.** Ventana de reloj ±120 s, nonce registrado con TTL
   (tabla `agent_nonces` con índice único y limpieza por cron existente) ⇒ anti-replay
   sin Redis y sin costo extra. Firma inválida ⇒ 401; timestamp fuera de ventana ⇒ 401.
4. **Premium, decidido solo en servidor.** Cada request revalida plan y suscripción
   (aprovechando el cache de entitlements de 60 s que ya existe). Sin PREMIUM activo ⇒
   **402** y el agente muestra "equipo bloqueado". El agente puede seguir leyendo su
   caché local, pero no puede materializar ni cotizar.
5. **Revocación y límite.** `print_agents` gana `revoked_at` y `last_seen_at`; el
   workspace tiene un máximo de dispositivos (p. ej. 3). Revocar invalida la clave al
   instante en el servidor, sin tocar la PC.

Por qué esto y no anti-tamper: el precio, el tarifario y el catálogo ya viven en el
servidor; un agente crackeado solo produce números de cobertura sin valor comercial.
Añadir ofuscación cuesta rendimiento y mantenimiento y no cambia el modelo de confianza.

### La gracia de 72 h no puede ser un contador local

Un "última verificación + 72 h" guardado en disco **sí es una vulnerabilidad**: se reinicia
solo con adelantar el reloj, y se vuelve indefinido restaurando una copia del archivo o de
la VM. Por eso la ventana no la lleva el agente, la lleva el servidor:

- El servidor emite un **token de entitlement firmado con Ed25519**,
  `{agent_id, workspace_id, plan, features:["print:agent"], iat, exp, nonce}`, con
  `exp = min(ahora + 72 h, fin del periodo pagado)`. Es una **caducidad absoluta**.
- Offline, el agente solo verifica la firma y compara `exp` con el reloj. **Nunca extiende
  nada localmente**: no existe el concepto de "renovar la gracia".
- Si la suscripción se cancela, el servidor simplemente deja de emitir tokens. El último
  token muere en su `exp` y el sobrepaso máximo es una sola ventana de 72 h, una vez.
- El token va atado a la clave pública del dispositivo: copiarlo a otra PC no sirve.
- **Anti-retroceso de reloj:** se guarda `max_seen_utc` (el mayor instante visto) en
  `agent-state.json`, en `HKCU\Software\MyPaper\PrintAgent` y en el `mtime` de un archivo
  testigo. Si el reloj del sistema es menor que `max_seen_utc − 5 min`, el agente se
  declara bloqueado. Sirve para detectar el truco barato, no para impedir a un
  administrador decidido, y eso es aceptable: lo que se protege no es el binario, es la
  capacidad de cobrar, y esa vive en el servidor.

Límite honesto: quien controle la máquina y guarde una instantánea de la VM puede volver a
un instante donde el token aún era válido. No hay defensa local contra eso, y no vale la
pena: al hacerlo pierde su propio historial de ventas y su outbox del POS. Lo que sí queda
cerrado es el caso realista —cancelar la suscripción y seguir operando—, que muere en 72 h
y no se renueva.

Migración: transición de 1 release aceptando el Bearer actual, con flag
`require_signature` por workspace; luego se vuelve obligatorio.

Cierre: tests de firma válida, firma inválida, replay, reloj desfasado y plan vencido.
Un agente sin suscripción no puede completar el flujo aunque tenga el binario y la clave.

## Fase 4 — UI Tauri v2 + Svelte (2-3 semanas)

Objetivo: retirar gpui sin tocar el core, y reutilizar el stack que ya se escribe a diario
en MyPaper.

Arquitectura:
- Nuevo crate `crates/desktop` (Tauri v2, MIT/Apache-2.0) que **enlaza** `mpa-agent`,
  `mpa-spool`, `mpa-sync` en proceso. No hay servidor HTTP local ni IPC extra: los
  comandos Tauri llaman al core directamente.
- Comandos `invoke`: `list_jobs`, `job_pages`, `page_preview`, `set_selection`,
  `complete_and_print`, `cancel_job`, `pair`, `doctor`, `tiers_sync`.
- Empuje de estado: el core ya expone un lock de 500 ms para la UI; se reemplaza por
  `app.emit("state", …)` desde el hilo del runtime, con el mismo lock discipline que
  hoy valida `lock_discipline.rs`.
- Frontend: Svelte 5 + Vite. Se reutilizan los design tokens y componentes del POS para
  que el agente se vea como MyPaper, no como una app aparte.
- Tray: `tray-icon` de Tauri (feature `tray-icon`) + notificación de trabajos retenidos.
- Autostart: `tauri-plugin-autostart` en lugar de la tarea programada, si el proceso
  deja de requerir elevación (ver Fase 5).
- `crates/ui` (gpui) queda congelado como referencia hasta que el nuevo UI tenga paridad:
  retener trabajos, lista de páginas, miniaturas, selección, destino, completar, cancelar.
- Dev loop: `cargo tauri dev` con HMR de Vite. Este es el cambio que realmente reduce el
  costo por cambio de UI.

Cierre: paridad funcional 1:1 con `crates/ui`, sin gpui en el binario distribuido.

## Fase 5 — Instalador "next, next, next" (1 semana)

Recomiendo **seguir con Inno Setup 6** en lugar del bundler de Tauri: ya tienes
`packaging/mpa.iss` funcionando, es gratis, y su bloque `[Code]` en Pascal permite la
verificación previa que pides. Tauri solo aportaría NSIS con menos control.

Qué debe verificar y resolver el instalador:

| Prerrequisito | Cómo se resuelve |
|---|---|
| Windows 10 1809+ x64 | `ArchitecturesAllowed` + chequeo de build; mensaje claro y aborto |
| WebView2 Runtime | Detección por registro (`HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients`); si falta, ejecutar el **instalador offline de WebView2 embebido** (~127 MB) en silencio. Sin internet, sin intervención |
| VC++ runtime | Se elimina la dependencia con `+crt-static` en el target MSVC: el binario no necesita redistribuible |
| Colas detectadas (USB, TCP/IP, WSD) | Se enumeran y se reporta cuáles quedan analizables; las de otro PC se marcan como no cubiertas |
| EMF spooling por cola | Verificación en `[Code]`; si una cola está en RAW, se ofrece corregirla (con reversión registrada) en lugar de dejarla sin análisis |
| Ghostscript / GhostPCL | Ya no se requiere (Fase 0/1) |
| Permisos de spool | Ver decisión D3 abajo |

- Un solo `.exe` de setup, `WizardStyle=modern`, español, con progreso y sin preguntas
  técnicas. Si todo está presente, son literalmente tres clics.
- Verificación posterior: al terminar, lanzar `mpa doctor --json` y mostrar un resumen
  (spooler accesible, backend de impresión, enlace, versión del motor de análisis).
- Auto-update: se conserva el canal minisign actual (`crates/update`); el instalador
  acepta `/VERYSILENT` y ya no roba foco.
- Prueba de aceptación: VM limpia Windows 10 y 11 **sin WebView2 y sin VC++**, instalar
  sin internet y completar un análisis de prueba.

## Fase 6 — Caché local de análisis (3-4 días)

Objetivo: no volver a analizar lo mismo y no perder un análisis si la subida falló.

- `%APPDATA%\mypaper-print-agent\analysis-cache\<sha256>-<engine.version>.json`,
  escritura atómica (tmp + rename, como ya hace `PairingStore::save`).
- Clave = sha256 del payload + versión del motor + dpi + umbrales. Si cambia cualquiera,
  se recalcula: nunca se sirve un precio con un motor viejo.
- Índice `index.json` con tamaño y último uso; LRU con tope configurable (por defecto
  500 MB o 2000 entradas), purga al arrancar.
- Reutilización: reimprimir el mismo archivo es cero recálculo y cero costo.
- Outbox: el análisis se guarda **antes** de intentar el POST; si falla, ya está en la
  cola existente y se reintenta. El operador nunca pierde el trabajo.
- CLI: `mpa cache stats`, `mpa cache clear`, `mpa cache gc`.
- Sin base de datos: un directorio content-addressed resuelve el caso y evita sumar
  SQLite al instalador.

## Fase 7 — Endurecimiento y piloto (1 semana)

- `cargo deny`, `clippy -D warnings`, nextest completo, bench del analizador.
- Tests de contrato de firma y de plan vencido (Fase 3).
- Test de instalador en VM limpia (Fase 5).
- Piloto en la papelería: una semana en sombra — el POS cobra y registra, la impresión
  real sigue igual que hoy.
- Documentación: README, HANDOFF y guía de instalación para el dueño.

## Secuencia y duración

    Fase 0  Fundaciones            2-3 días
    Fase 1  Analizador v2          1.5-2 semanas   <- desbloquea todo
    Fase 2  Precio por área        1 semana
    Fase 3  Firma y Premium        1 semana
    Fase 4  UI Tauri + Svelte      2-3 semanas
    Fase 5  Instalador             1 semana
    Fase 6  Caché local            3-4 días
    Fase 7  Endurecimiento/piloto  1 semana
    ------------------------------------------
    Total                          ~9-10 semanas (1 persona)

Fases 4 y 5 pueden solaparse con 2 y 3 si hay una segunda persona. Fase 1 no se puede
paralelizar con nada: es la que define el número que se cobra.

## Mejoras que propongo sobre lo que pediste

1. **Quitar Ghostscript es la mejora más grande del plan.** No solo por licencia: hoy el
   instalador depende de que exista una instalación de terceros y el número que se cobra
   depende de su versión. pdfium lo fija y lo hace reproducible.
2. **Separar el proceso del spool de la UI (servicio + named pipe).** Hoy `mpa-ui.exe`
   exige `requireAdministrator`: UAC en cada arranque y una ventana con privilegios
   totales. Lo correcto es un servicio Windows (LocalSystem) que habla por named pipe con
   una UI sin elevación. Cuesta ~1 semana más y mejora seguridad, arranque y actualización.
   Se puede posponer sin bloquear (ver D3).
3. **`AREA` como métrica explícita**, no como reinterpretación de `SUM_CMYK`. Así el
   tarifario de área convive con el de tinta y se puede comparar en la misma cotización.
4. **Firma las decisiones con tests de vectores compartidos** (un JSON que leen Rust y
   TS). Es lo que evita la deriva de precios que ya casi ocurre hoy.
5. **No persigas anti-tamper.** El servidor es el que cobra; un agente alterado no puede
   facturar. Invertir en ofuscación sería gasto sin retorno.
6. **Caché content-addressed en vez de SQLite.** Menos dependencias, menos instalador,
   mismos beneficios.

## Decisiones resueltas (2026-10-25)

D1. **Firma de código: se pospone.** Se publica el instalador sin Authenticode, con
    SHA256 + firma **minisign** (ya implementada en `crates/update` y en el workflow). El
    sitio de descarga muestra el hash y una nota de cómo continuar ante el aviso de
    SmartScreen ("Más información → Ejecutar de todas formas"). El paso de firma queda
    detrás de una variable de entorno en `build.ps1`, para poder añadir el certificado
    después sin tocar nada más.
    Efecto en Fase 5: no se compra certificado; sí se documenta el aviso.

D2. **Se analiza todo lo que salga por la cola de impresión del PC donde está el agente,
    sea impresora local (USB) o de red (TCP/IP, WSD, o compartida).** No se analiza nada
    que no pase por ese PC: copias hechas en el panel de la impresora, impresión directa
    por USB a la impresora, o trabajos impresos desde otro equipo. Eso es físicamente
    inobservable desde el agente y así se documenta.

    El detalle que importa: una impresora de red no cambia el origen del trabajo —el
    spooler del PC lo genera igual— pero **sí cambia el formato que deja en el spool**. Con
    "EMF spooling" el archivo retenido es EMF y se puede rasterizar; con datatype RAW el
    archivo ya es PCL/PostScript del driver y no hay renderizador libre para eso. Por eso
    el agente no depende del formato: **fuerza EMF spooling** en las colas que administra.

| Escenario | ¿Se analiza? | Cómo |
|---|---|---|
| Impresora local USB | sí | EMF en el spool → GDI → raster |
| Impresora de red TCP/IP o WSD | sí | mismo camino: el spool es local |
| Cola compartida desde otro PC (`\\servidor\impresora`) | no garantizado | el spool vive en el servidor; se detecta y se reporta |
| Copia desde el panel de la impresora | no | no pasa por el PC |
| USB directo a la impresora | no | no pasa por el PC |
| Word, Excel, PDF, PhotoScape, etc. | sí | todos terminan en la misma cola |

    Formatos que el analizador entiende: **EMF, PDF e imágenes**. PostScript y PCL solo
    aparecen si una cola queda en RAW; el instalador lo evita y `doctor` lo reporta por
    impresora. Nada de esto requiere Ghostscript.

D3. **Se mantiene el ejecutable elevado** (`requireAdministrator` + tarea programada).
    No hay servicio ni named pipe. Efecto en Fase 4/5: la app Tauri corre elevada, así que
    el WebView2 hereda el token de administrador. Contramedida obligatoria: la UI carga
    **solo assets locales embebidos**, CSP cerrada, sin navegación externa, sin
    `dangerousRemoteDomainIpcAccess` y validación de todos los comandos en Rust.

D4. **Premium vencido bloquea todo.** Sin suscripción activa: no se analiza, no se cotiza,
    no se materializa, no se retiene con precio. Sí se permite ver el registro local,
    `doctor`, emparejar y actualizar. Regla de seguridad que no se negocia: **un trabajo
    ya retenido se libera y se imprime sin gestión tras 10 minutos**, para que una
    suscripción vencida nunca deje a un cliente sin su impresión.

D5. **Gracia offline de 72 h con caducidad absoluta, no con temporizador local.** Detalle
    en Fase 3. La ventana no se puede renovar desde el equipo.

D6. **Página en blanco: no se cobra** (`blankPageFree=true`, sin excepción). Y **gris
    cuenta como color sólido**, en color y en ByN.

    Esto obliga a corregir la semántica actual: `PrintPricing::has_color_ink` devuelve
    `false` cuando `c + m + y == 0`, así que una página gris compuesta solo de K (lo
    normal) se factura hoy como ByN. La regla nueva, derivada del clasificador por píxel:

| Clase | Condición | Se cobra con |
|---|---|---|
| blanco | `coverage_nonwhite ≤ 0.4 %` | nada (gratis) |
| color | `max(C,M,Y) > 0.2 %` | escalera COLOR |
| gris | hay tinta, sin croma, `pure_black_frac < 0.5` | escalera COLOR (gris = color sólido) |
| mono | hay tinta, sin croma, `pure_black_frac ≥ 0.5` | tarifa ByN |

    `pure_black_frac` es la fracción de píxeles con tinta donde K ≥ 95 % y croma ≈ 0, es
    decir negro puro. Es lo único que distingue "texto negro" de "gris". El flag
    `greysAsColor` se conserva por compatibilidad, pero con `true` (como hoy) la fila
    `gris` va siempre a COLOR.

## Riesgos

| Riesgo | Impacto | Mitigación |
|---|---|---|
| pdfium y el corpus no coinciden al 0.05 pp | alto | Fase 1 arranca con el test dorado; si falla, se ajusta el mapeo de espacios de color (misma convención sRGB que el REA) |
| El tramo real de CyberPlanet no era área | medio | el preset es editable; se valida con facturas de caja, no se asume |
| Migración Tauri rompe el flujo de retención | alto | `crates/ui` se conserva hasta paridad; el core no cambia |
| Sin firma Authenticode, adopción | medio | documentar el aviso + hash publicado (D1) |
| Servicio/named pipe añade superficie | medio | se pospone (D3) y se hace con ACL por SID de usuario |

## Estado de implementación (2026-10-25)

Hecho y verificado en este repo:

- `crates/analyzer/src/classify.rs` — medición por píxel completa (11 métricas +
  `PageClass`), réplica término por término de `render_truth.js`, con `Thresholds` y
  `ENGINE_VERSION` versionados. 9 tests unitarios, incluidos los dos que fijan las reglas
  del dueño: gris factura en la escalera de color y blanco no se cobra.
- `crates/analyzer/tests/parity_rea.rs` — **paridad sobre las 35 páginas del corpus**:
  todas las métricas dentro de 0.05 pp y clase idéntica en las 35. Fixtures copiados a
  `crates/analyzer/tests/fixtures/rea/` (3.6 MB, 35 PNG + `render_truth.json`).
- `PageAnalysis` ahora lleva `metrics: Option<PageMetrics>`; las rutas de imagen ya miden
  con el clasificador nuevo.
- Se eliminó el estimador naive (`coverage_of_image`/`coverage_of_file`): queda **una sola**
  forma de medir. Ese era el defecto que cobraba distinto el mismo contenido según el
  formato de entrada.
- `cargo clippy --workspace --exclude mpa-ui` limpio; 128 tests en verde.

Hecho después (misma fecha, segunda pasada de Fase 1):

- `crates/analyzer/src/pdf.rs` — rasterizado PDF con **pdfium** (BSD-3) vía
  `pdfium-render` (MIT/Apache). Instancia única serializada, búsqueda de la librería en
  `MPA_PDFIUM` → junto al ejecutable → PATH. Reemplaza a Ghostscript en la ruta PDF.
- `crates/analyzer/tests/pdf_render.rs` — **end-to-end**: los 30 PDF del corpus
  renderizados con pdfium a 150 dpi y medidos. Las 35 páginas clasifican igual que la
  referencia; canales dentro de 1 pp y área dentro de 6 pp (la diferencia es del
  rasterizador, no de la medición: 5 pp máx. en un halftone 20%, 2.4 pp en texto denso).
- Ruta EMF medida con `rasterize_page` + clasificador en Windows (verificada con
  `cargo check --target x86_64-pc-windows-gnu`).
- `scripts/repair_rea_corpus.py` — el corpus del estudio tiene el catálogo apuntando a un
  objeto vacío; pdfium sintetiza una página en blanco. Se reparan copias para los tests.
  Documentado en `crates/analyzer/tests/fixtures/rea/README.md`.
- `pdfium` habilitado desde `mpa-agent` y `mpa-cli`; sin librería disponible cae al camino
  anterior en vez de fallar el trabajo.

Pendiente de Fase 1:

- Provisionar `pdfium.dll` en el instalador y en el workflow de release (7.6 MB).
- Retirar `gs.rs`/`convert.rs` y el flag de Ghostscript una vez que la DLL esté en los
  releases; hoy siguen como red de seguridad para PostScript/XPS/PCL.

### Cierre de Fase 1 (misma fecha)

- `gs.rs`, `convert.rs` y `subset.rs::print_pdf_to_winprinter` **eliminados**: el binario
  ya no contiene ninguna ruta que invoque Ghostscript. PostScript, XPS y PCL se rechazan
  por nombre.
- `print.rs` nuevo: impresión por raster (`StartDocW`/`StretchDIBits`) de las páginas
  medidas. Reemplaza `gs -sDEVICE=mswinpr2`, así lo que se imprime es lo que se midió.
- `reprint.rs` reescrito: renderiza las páginas seleccionadas a 300 dpi y las imprime; ya
  no extrae un PDF con Ghostscript. Las opciones de papel/dúplex aún no se aplican en esa
  ruta (el entry las conserva para auditoría).
- `doctor` reporta pdfium en lugar de ghostscript/gpcl6.
- `packaging/fetch-pdfium.ps1` + instalador + workflow: `pdfium.dll` viaja junto a los
  binarios. Los prerrequisitos quedan en **Windows + WebView2**.

### Fase 2 — precio por área (cerrada salvo el preset)

- `PrintMetric::Area` en el dominio con `metric_coverage_with_area`; sin medición cobra 0
  en vez de adivinar, y se acota a 0-100.
- `dearest_tier`/`priced_coverage` reciben el área medida, así la regla "gana el más caro"
  sigue valiendo cuando el conjunto de color factura por área.
- El agente conserva `PageArea` (área + clase) y la envía como `coverageNonwhite` y
  `pageClass`, solo cuando midió píxeles. Sin eso, un workspace en AREA habría cobrado 0.
- POS: `AREA` en el enum, en la métrica de cobro, en la migración `print_metric` y en el
  selector del editor de tarifas. `printJobPage` acepta los campos nuevos como opcionales.
- **Falta**: el preset "CyberPlanet 6.5" (6 tramos + ByN $1.50 como seed configurable).
  Es datos, no código: se puede cargar desde `print-tiers` sin tocar el esquema.

Riesgo de este incremento: **ninguno de los ítems altos**. Cambia la medición de imágenes
(contrato interno, no publicado) y añade un campo opcional; no toca datos persistidos,
seguridad, contratos externos ni concurrencia. `Risk: none`.

### Fase 3 — binding agente ↔ MyPaper (primera mitad)

Hecho y verificado:

- Agente: `DeviceKey` (Ed25519) generado al emparejar, secreto guardado junto a la clave en
  `agent.json`, firma por petición con `x-mpa-ts` / `x-mpa-nonce` / `x-mpa-sig`.
- Cadena canónica `METHOD\npath\nts\nnonce\nsha256(body)`, con un **vector de prueba
  compartido** entre Rust y TypeScript: si alguien la cambia en un lado, fallan los dos.
- Servidor: verifica la firma con `node:crypto`, ventana de 120 s y **nonce de un solo uso**
  (índice único `(agent_id, nonce)`: es el insert el que decide un replay, así dos copias
  simultáneas no pueden ganar las dos).
- Emparejamientos anteriores (sin llave pública) siguen funcionando con Bearer: cero
  ruptura para instalaciones existentes.
- El gate PREMIUM ya existía en todos los endpoints con valor (`jobs`, `quote`, `tiers`,
  `open-tickets`, `heartbeat`) y sigue siendo la única autoridad: el agente no decide nada.

Verificación: 7 tests nuevos en Rust, 8 en TS (firma honesta, cuerpo/ruta/sello/nonce
alterados, firma ajena, llave mal formada, ventana de reloj, query en la ruta).

Pendiente de Fase 3:

- Token de entitlement con caducidad absoluta (72 h) y anti-retroceso de reloj; hoy el
  agente depende de la respuesta por petición, que es correcta pero no permite operar
  sin red.
- Límite de dispositivos por workspace (hoy solo revocación por `revoked_at`).
- Limpieza programada de `print_agent_nonces` (el índice ya existe; falta engancharla al
  cron de mantenimiento).

#### Segunda mitad de Fase 3 — gracia offline (hecha)

- Servidor: `GET /api/agent/entitlement` emite un token Ed25519 firmado con
  `exp = min(ahora + 72 h, fin del periodo pagado)`; sin PREMIUM no emite. La ventana la
  fija el servidor y **no se renueva desde el equipo**: al cancelar, el último token
  simplemente vence.
- Agente: `verify_with_public` (llave compilada con `option_env!`, override por
  `MPA_ENTITLEMENT_PUBKEY`) y `ClockGuard`, un máximo visto del reloj que bloquea si el
  sistema retrocede más de 5 minutos.
- CLI: `mpa entitlement` renueva y guarda; `mpa entitlement --status` verifica el token
  local sin red.
- Los tokens de prueba los emite **la implementación TypeScript**, así que cada suite
  verifica el formato del otro lado, no el suyo.

Probado de punta a punta por CLI: token válido → `ok, 57505 h restantes`; reloj retrasado
→ `bloqueado: clock rollback`; token caducado → `bloqueado: token expired`.

Sigue pendiente de Fase 3: límite de dispositivos por workspace, purga programada de
nonces, y que el runtime del agente entre en modo suspendido al fallar la gracia (hoy la
verificación existe pero solo la ejecuta el comando).

## Primer entregable concreto

Fase 1 con el test dorado corriendo sobre los 35 páginas del REA y el analizador sin
Ghostscript. Es lo único que hay que hacer antes de decidir cualquier otra cosa, y es lo
que convierte "copiar a CyberPlanet" en un número propio, reproducible y defendible.
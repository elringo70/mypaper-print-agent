# MyPaper Print Agent — plan v3 (captura por cola IPP)

Reemplaza al plan v2 en todo lo que toca **captura y flujo**. El v2 sigue válido en
analizador, tarifario, firma y entitlement.

## Por qué hay un v3

El v2 capturaba mirando la cola de la impresora física. Eso entrega el flujo del driver
—PCL crudo— y medirlo exige GhostPCL (AGPL) o licencia comercial. Se probó con la Konica
real: `unsupported spool format: Pcl`, cero páginas.

La captura se movió a una **cola propia por IPP**. No es un driver: Windows la renderiza
con su IPP Class Driver y, al anunciar sólo `application/pdf`, entrega el documento como
PDF. Probado con un documento de Word: llegó como PDF 1.7 de 2 páginas y midió K=4.77 % y
K=1.67 %. Por eso Ghostscript y el watcher de spool desaparecen del camino principal.

## El flujo, tal como lo definió el dueño del producto

1. El cliente manda imprimir desde cualquier programa.
2. El agente **retiene** el trabajo, lo analiza y muestra el resultado.
3. El usuario **autoriza**: elige modo, páginas y destino, y sólo entonces se imprime.
4. Al autorizar, el trabajo se cobra en el ticket abierto que elija.

**Cambio semántico que hay que decir en voz alta:** con IPP no se "pausa" nada en el
spooler. Windows entrega el documento y lo da por impreso; el que no imprime hasta que le
digan es el agente. Es mejor que pausar: no hay carrera con el spooler, no se pierde el
trabajo si el servicio se reinicia (el PDF ya está en disco) y no hace falta privilegio
para tocar trabajos de otros.

## Decisiones cerradas

- **D1.** El punto de captura es la cola IPP `MyPaper`, creada por el instalador con
  *Microsoft IPP Class Driver*. Nada de driver propio ni de firma de driver.
- **D2.** El agente anuncia **sólo `application/pdf`**. Ofrecer PWG Raster haría que
  Windows rasterice y perderíamos la resolución que se mide.
- **D3.** Las impresoras físicas **ya están instaladas y configuradas antes** de instalar
  el agente: una para blanco y negro, otra para color. El instalador no las crea; pide
  cuál es cuál y verifica que existan.
- **D4.** El modo (B/N o Color) lo elige el usuario en la ventana, con dos botones arriba
  a la derecha, y **el precio se recalcula ahí mismo**.
- **D5.** Un solo botón de acción: **Imprimir**. El cobro ocurre por la lista de tickets
  abiertos: si hay ticket elegido, las líneas se envían a ese ticket; si no, queda como
  borrador y no se cobra.
- **D6.** Las páginas en blanco no llevan tóner; el recargo de hoja es una decisión aparte
  y hoy se cobra (3.00 por hoja en el espacio de pruebas).
- **D7.** Licencias libres o gratuitas, sin excepciones. `cargo deny` lo hace cumplir.

## Lo que ya existe (no se rehace)

| Pieza | Estado |
|---|---|
| Analizador por página (11 métricas, clase, área) | hecho, paridad 35/35 con el REA |
| Tarifario por tramos, AREA/K_ONLY/SUM_CMYK, ambos modos | hecho |
| Firma Ed25519 agente↔POS + entitlement 72 h | hecho |
| Endpoint IPP (`crates/ipp`) | hecho, verificado con Word |
| Reimpresión GDI a 300 dpi | hecho, llegó a la cola física |
| Borrador cobrado en MyPaper (idempotente por contenido) | hecho, verificado contra el POS |
| Instalador Inno | hecho, sin la cola IPP ni los roles |

## Fases

### F1 — El agente captura por IPP (deja de ser un comando suelto)

Hoy `mpa ipp` es un proceso aparte. Pasa a ser el camino del agente:

- el servicio arranca el endpoint IPP junto con el resto del agente;
- cada trabajo capturado entra a la **misma cola de trabajos** que consume la ventana
  (hoy la cola viene del watcher de spool);
- el estado del trabajo gana un paso: `Capturado → Analizado → Autorizado → Impreso`.

Aceptación: imprimir desde Word con el agente corriendo deja un trabajo visible en la
ventana, sin consola y sin `--reprint` a mano.

**Hecha.** `AgentCore::ingest_captured` construye el trabajo y reutiliza `record_analysis`,
así que un documento capturado se mide, clasifica y tarifa igual que uno vigilado: un solo
mapeo, dos entradas. `AgentRuntime::start_capture` levanta el endpoint con el agente y
`mpa-desktop` lo ata al arrancar (puerto ocupado se registra, no mata el proceso). Los ids
de trabajo van con el nombre de cola, así que no colisionan nunca con ids del spooler.

Test por socket real: un PNG enviado como Print-Job aparece en `snapshot()` con una página,
25 % de área y su archivo en disco.

Falta de esta fase: el botón que autoriza. `complete_job` todavía asume un trabajo retenido
en el spooler, así que para un trabajo capturado hay que saltarse cancelar/reanudar e ir
directo a la reimpresión — eso es F2.

### F2 — Ventana de autorización (lo que muestra el screenshot)

La ventana por trabajo, con la disposición del ejemplo:

- **Detalles del documento**: nombre, páginas, tamaño de hoja, copias.
- **Total en grande**, que cambia al cambiar de modo.
- **Detalles de tarifación**: una línea por página con su costo y casilla.
  *Seleccionar todas las páginas*.
- **Vista previa** de la página, con negro / color / blanco en porcentaje.
- **Selector de impresora** con las dos físicas y opciones de dúplex.
- **Dos botones de modo arriba a la derecha**: B/N y Color. Al pulsarlos se pide
  cotización nueva y el total se repinta.
- **Lista de tickets abiertos** del espacio de trabajo, para enviar ahí las líneas.
- **Imprimir** (y Cancelar). Sin ticket elegido, imprime y deja borrador.

Aceptación: cambiar de B/N a Color cambia el total con el precio del servidor, no con una
cuenta local.

**Hecha.** `authorize_captured` no cancela ni reanuda nada —no hay trabajo en el spooler—
y siempre encola la reimpresión; se niega si el trabajo ya fue autorizado (cobraría dos
veces) o si no hay documento en disco. `commands::quote` pide al servidor el precio de los
dos juegos de reglas para las páginas incluidas, así que el total y el costo por página son
del servidor y no de una cuenta local.

La ventana quedó con el modo arriba a la derecha, total grande, tarifación por página con
casillas y "seleccionar todas", vista previa con negro/color/blanco y la lista de tickets
abiertos como destino.

Verificado: dos tests de integración (captura medida; autorización encolada hacia la
impresora de color con ticket, segundo intento rechazado), `svelte-check` 0/0 y el bundle
nuevo `index-Bvq3vvZS` incrustado en `mpa-desktop.exe`.

Falta: probarlo con una impresión real (el barrido del runtime es el que convierte
`reprint_pending` en papel) y que la ventana muestre el error del servidor cuando no hay
emparejamiento.

### F3 — Tickets abiertos en la ventana

El agente ya refresca tickets cada 30 s; falta exponerlos en la ventana con los datos que
el operador reconoce (folio, cliente, total) y enviar el trabajo al elegido.

Aceptación: imprimir con ticket elegido deja las líneas en ese ticket y el trabajo en
`MATERIALIZED`.

**Hecha.** El POS mandaba `cashierId` y ningún cliente, así que la lista mostraba un UUID.
`findOpenTickets` ahora une cliente y cajero (ambos son filas de `users`, el cajero
aliasado) y devuelve nombres; la ventana etiqueta `#50 · Cliente · Cajero · $104`.

Verificado contra el POS local: emparejar, mandar 2 páginas al ticket #50 y materializarlas
llevó el ticket de 104.00 a 107.00 con una línea real de «IMPRESIÓN COLOR». La línea y el
total se restauraron después.

También se apagan «Imprimir» y «Cancelar» en cuanto el trabajo deja de estar pendiente: un
segundo clic ya no puede encolar otra impresión.

### F4 — Instalador con roles y cola

- Página nueva del asistente: **¿cuál imprime en blanco y negro? ¿cuál en color?** con la
  lista de impresoras ya instaladas. Si no hay dos, avisa y deja continuar en modo
  "una sola impresora".
- Crea la cola `MyPaper` con `Microsoft IPP Class Driver` apuntando a
  `http://127.0.0.1:8631/ipp/print` y comprueba que el agente responde antes de terminar.
- Deja el agente como tarea elevada; el endpoint escucha sólo en localhost.

Aceptación: en una VM limpia, tres clics y luego imprimir desde Word funciona sin tocar
PowerShell.

**Hecha (pendiente de la prueba en VM).** El asistente gana una página con dos listas
desplegables tomadas de las impresoras que Windows ya tiene: cuál es B/N y cuál color; por
defecto las dos primeras, que es como suele estar montada una papelería.

`packaging/setup-agent.ps1` hace el resto y es idempotente: guarda los roles con
`mpa printers set`, arranca el agente si no corre, espera a que el endpoint conteste, crea
la cola `MyPaper` con el IPP Class Driver y comprueba que quedó. Corre en `ssPostInstall`
y **su código de salida se mira**: si la cola no apareció, el asistente lo dice en vez de
dejar un agente que no captura nada.

Verificado en el espejo: el script guardó B/N = KONICA, encontró el endpoint en 8631, vio
la cola existente y salió con 0. ISCC compila la página nueva (11.8 s).

Falta: instalación real en una VM limpia con dos impresoras (una de ellas de color), y
descargar el runtime offline de WebView2 para el paquete definitivo.

### F5 — Retirar el camino viejo

- El watcher de spool y la pausa de trabajos salen del flujo principal.
- Se quedan sólo como diagnóstico (`mpa capture`) mientras dure la transición.
- Documentar en la ventana qué NO se captura: impresión directa a la física, copias del
  panel, USB y "guardar como PDF".

Aceptación: el binario no abre ningún handle de trabajo ajeno en operación normal.

**Hecha.** `legacy_spool_watch` (falso por defecto) decide si se vigila el spooler. La
ventana arranca con una lista blanca vacía y lo dice en el log: en operación normal el
agente no abre ningún handle de trabajo ajeno. `mpa watch` y `mpa capture` siguen
existiendo como diagnóstico, y la bandera devuelve el vigilante a quien lo necesite.

La ventana ahora dice qué queda fuera de alcance, en lugar de dejarlo a la imaginación:
copias desde el panel, USB y lo que se manda directo a otra impresora.

### F6 — Piloto en sombra

Medir sin cobrar durante una semana en la papelería, comparar contra lo que el operador
espera y contra lo que reporta CyberPlanet en la misma máquina para las mismas páginas.

**Instrumentado.** El agente escribe un diario en `ipp-inbox\pilot.jsonl`: una línea por
trabajo con sus páginas (clase, área, CMYK) y lo que habría costado en cada juego de reglas.
El precio se pide al servidor; si no está disponible, el trabajo queda registrado como «sin
precio» en vez de inventarlo. `mpa pilot report` lo resume:

```
2026-10-26T10:00:00Z  cotizacion.docx  2 pág.  B/N $9.00   Color $9.00
2026-10-26T11:30:00Z  foto.png         1 pág.  B/N $11.00  Color $48.00
2026-10-26T12:00:00Z  sin-servidor.pdf 1 pág.  B/N sin precio  Color sin precio

trabajos 3 · páginas 4 · blanco 0 · mono 2 · gris 1 · color 1
total B/N $20.00 · total color $57.00 · 1 sin precio
```

**Cómo correrlo** (esto ya es del operador, no del código):

1. Una semana imprimiendo normal por la cola `MyPaper`, sin tocar el cobro.
2. Al cerrar cada día, `mpa pilot report --days 1` y comparar contra lo que el sistema
   anterior cobró por esas mismas hojas.
3. Al final, `mpa pilot report` y revisar tres cosas: páginas que el agente vio y el
   sistema viejo no cobró (o al revés), trabajos «sin precio», y diferencias de clase
   (gris tratado como color, blancos cobrados).

Lo que puede salir mal y qué significa: un trabajo «sin precio» es una caída del servidor,
no un error de medición; una página de menos es alguien imprimiendo fuera de la cola; una
diferencia de clase es una regla del tarifario, no del analizador.

## Riesgos

| Riesgo | Mitigación |
|---|---|
| El operador imprime a la Konica y no a `MyPaper` | La ventana lo dice; el instalador deja la cola como predeterminada |
| Windows Protected Print retira drivers de terceros | El endpoint IPP no es un driver: sobrevive al cambio |
| El precio mostrado y el cobrado divergen | El total sale de `/api/agent/quote`, nunca de una cuenta local |
| Doble cobro al reintentar | Idempotencia por hash del contenido (ya probada) |
| Trabajo perdido si el agente cae | El PDF se guarda en disco antes de responder al cliente |

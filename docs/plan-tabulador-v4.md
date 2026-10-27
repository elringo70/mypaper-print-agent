# Plan v4 — tabulador tipo CyberPlanet, producto por nivel y cierre del flujo

Este plan cierra lo que quedó abierto del plan v3 (F1..F6 hechas) y cubre los cambios
pedidos sobre el tarifario, el instalador y la validación entre las dos aplicaciones.

## Estado de partida (verificado, no supuesto)

| Pieza | Estado |
|---|---|
| Captura por cola IPP + medición + ventana de autorización | hecha y probada con Word |
| Cobro por área y por canal, ambos modos | hecho |
| Niveles en base de datos (`print_tiers`) | ya existían |
| Código de barras sin guiones (`IMPBW50`, `IMPCL50`) | hecho en este corte |
| Página en blanco: no se cobra y no se imprime | hecho en este corte |
| Firma Ed25519, nonces, entitlement, tope de equipos, RLS | ya existían |
| Validación "documento completo llegó al servidor" | **no existe** |
| Recargo por tamaño de papel | **no existe** |
| Bloqueo de instalación sin dos impresoras | **no existe** |
| Bandeja: cerrar oculta / arranque minimizado | por verificar |

## Decisiones cerradas

- **D1. El tabulador se pide como "Hasta %"**, igual que CyberPlanet: el mínimo de un
  renglón es el máximo del anterior. En la base se sigue guardando `min_coverage` y
  `max_coverage` (el motor de precios no cambia); el mínimo se **calcula** al guardar.
  Un nivel con "hasta 40" después de uno con "hasta 20" queda 20–40.
- **D2. El primer renglón empieza en 0.** Si se edita un nivel intermedio, el servicio
  reajusta el mínimo del siguiente para que la escalera no tenga huecos ni traslapes.
- **D3. Cada nivel sigue siendo un producto** de la categoría `IMPRESIONES`, con código
  `IMP` + modo + hasta (`IMPBW50`, `IMPCL100`), sin separadores.
- **D4. La página blanca no se cobra y no se imprime.** Ya implementado. Si el operador
  quiere conservar la paginación en dúplex, se cambia un filtro y se vuelve a medir;
  hasta entonces manda la regla dicha.
- **D5. El recargo por tamaño es un porcentaje sobre el precio del nivel**, configurable
  por workspace y por tamaño de papel, y **se materializa en `unit_price`** de la línea
  al crear el trabajo. Un cambio futuro del porcentaje no reescribe facturas viejas.
- **D6. El agente sólo opera si hay una impresora de color y una de blanco y negro**, y
  el POS sólo ofrece el módulo si hay al menos un nivel de color activo.
- **D7. "Borradores recientes" pasa a llamarse "Análisis recibidos"**, que es lo que de
  verdad prueba: que la medición llegó al servidor.

## Fases

### F1 — Escalera "Hasta %" en el POS

- `PrintTierService.create/update`: recibe `maxCoverage` y calcula `minCoverage` como el
  máximo del nivel anterior del mismo modo (ordenado por `sort_order`/`max`); valida que
  la escalera no se cruce y reajusta el siguiente nivel.
- `PrintTierModal`: un solo campo **Hasta %**, con la escalera visible en una tabla
  (`Hasta | Precio`) mientras se escribe, y el código de barras resultante al lado.
- Migración de los niveles existentes: recalcular `min_coverage` con la regla nueva y
  renombrar el producto (`IMPBW…`) actualizando su código de barras.

Aceptación: crear "hasta 21" después de "hasta 5" guarda 5–21, y el producto queda
`IMPBW21`. Un "hasta" menor o igual al anterior se rechaza con mensaje.

**Hecha.** El modal captura un solo "Hasta %" y muestra debajo el rango resultante; el
servicio calcula el piso con `ladderFloor` y el motor de precios no cambió.

Migración aplicada sobre la base de desarrollo: la escalera COLOR quedó
0–20–40–60–80–100 con códigos `IMPCL20`…`IMPCL100`. El nivel de B/N conservó su código
`12345` porque es un SKU real del catálogo, no un código generado por el módulo — el script
sólo reescribe códigos que empiezan por `IMP`.

### F2 — Diseño del modal igual al resto de la app

- Sustituir el modal por el patrón que ya usan productos y clientes: mismo ancho, misma
  cabecera, inputs con `label` arriba, errores por campo, botonera abajo a la derecha.
- La tabla de la escalera usa el mismo `table table-sm` de la página de niveles.

Aceptación: `svelte-check` 0/0 y el modal no introduce estilos nuevos fuera de los tokens
existentes.

### F3 — Precondiciones del módulo

- Instalador: si no hay al menos una impresora de cada rol, el asistente **se detiene**
  en la página de roles con el motivo (hoy permite continuar).
- POS: la página de niveles avisa y deshabilita la creación si no hay ningún nivel de
  color activo; el endpoint del agente responde 409 con el motivo en vez de cotizar 0.

Aceptación: en una VM con una sola impresora, el instalador no deja terminar y dice por
qué. Sin niveles de color, el agente no registra trabajos.

**Hecha.** `setup-agent.ps1` comprueba antes de tocar nada: al menos dos impresoras, ambos
roles elegidos, distintos y existentes, cada uno con su código de salida. El asistente
bloquea «Siguiente» en la página de roles con las mismas dos reglas, así que el operador se
entera ahí y no al final. En el POS, `createDraft` rechaza el trabajo si el tabulador no
tiene ningún nivel de color activo.

Verificado en el espejo: sin `-Color` sale 7 («faltan los roles»), con una impresora
inexistente sale 9 («no está instalada»).

### F4 — Bandeja y arranque

- Verificar que cerrar la ventana la oculta (no la termina) y que el proceso sigue
  capturando; el icono debe quedar en la bandeja de iconos ocultos.
- Añadir "Iniciar minimizado" al arranque automático, para que el login no abra la
  ventana encima de todo.
- El menú de la bandeja: Abrir, Estado (texto), Salir.

Aceptación: cerrar con la X deja el proceso vivo y capturando; Salir desde la bandeja
termina el proceso.

**Hecha, y encontró un fallo serio.** La tarea de inicio apuntaba a **`mpa-ui.exe`**, el
shell de gpui retirado: en una instalación nueva habría arrancado un binario que ya no
existe, y el agente se vería instalado sin correr nunca. Ahora apunta a `mpa-desktop.exe` y
pasa `--minimized`.

Cerrar y minimizar ocultan la ventana a la bandeja (Windows reporta una ventana minimizada
como 0×0). El proceso sigue capturando; sólo «Salir» del menú de la bandeja lo termina.
Verificado: desktop compila en Windows y el instalador compila (12.2 s).

### F5 — Recargo por tamaño de papel

- Tabla `print_media_surcharge` (workspace, media IPP, porcentaje). Semilla: carta 0 %,
  oficio 10 %, doble carta 25 %, tabloide 40 %.
- El agente ya recibe `media` en los atributos del trabajo IPP; lo guarda en el
  `OutboxEntry` y lo manda en el payload.
- El POS aplica el porcentaje sobre el precio del nivel y escribe el resultado en
  `unit_price` de la línea.

Aceptación: la misma página a oficio cuesta 10 % más que a carta, y la línea guardada
conserva el precio con el que se cobró.

### F6 — Validación de documento completo

- El payload del trabajo lleva `pageCount` declarado y `sha256` del documento.
- El servidor rechaza con 409 si llegan menos páginas que las declaradas o si el hash no
  corresponde al tamaño reportado; guarda ambos para auditoría.
- El agente marca el trabajo como **confirmado** sólo cuando recibe `jobId` +
  `pageCount` de vuelta, y lo muestra en la ventana.
- `mpa doctor` compara lo capturado en disco contra lo registrado en MyPaper.

Aceptación: quitar una página del payload produce 409 y el trabajo queda sin confirmar,
visible en la ventana en vez de cobrado a medias.

### F7 — "Análisis recibidos" y limpieza

- Renombrar la sección y añadir columnas: código corto de idempotencia, modo y si está
  confirmado.
- Filtro por estado (`PENDING` / `ATTACHED` / `MATERIALIZED` / `CANCELLED`).

Aceptación: un trabajo que falló al registrarse se distingue de uno que nunca llegó.

## Riesgos

| Riesgo | Mitigación |
|---|---|
| Migrar códigos de barras rompe productos ya usados en ventas | Sólo se renombra el código; el `product_id` de las líneas no cambia |
| La escalera calculada deja huecos al borrar un nivel | El servicio reajusta el siguiente, y la validación de solapamiento ya existe |
| Sin validación de documento completo se cobra de menos | F6 antes de salir de sombra |
| El recargo por tamaño se aplica dos veces si el operador también lo puso en el precio del nivel | El porcentaje se aplica una sola vez, en el servidor, y queda escrito en la línea |

## Orden de trabajo

1. F1 y F2 juntas (mismo archivo, misma prueba).
2. F6 antes que F5: cobrar de menos por un payload incompleto es peor que un recargo
   mal puesto.
3. F3 y F4 (instalador y bandeja).
4. F5 y F7.

## Lo que necesito decidir contigo

- **Paginación con páginas en blanco**: hoy se saltan (la siguiente sube). ¿Se queda así
  o se imprimen en blanco para no desplazar el documento?
- **Porcentajes de tamaño**: propongo carta 0 %, oficio 10 %, doble carta 25 %,
  tabloide 40 %. ¿Números tuyos?
- **Niveles existentes**: ¿los migro con la regla nueva o los recreas tú desde la
  pantalla?

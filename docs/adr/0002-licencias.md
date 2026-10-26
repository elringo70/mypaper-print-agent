# ADR 0002 — Licencias: solo lo que se puede redistribuir sin costo

**Estado:** aceptada · **Fecha:** 2026-10-25

## Contexto

El agente se distribuye como binario cerrado a papelerías. Cualquier licencia que
obligue a publicar el código del producto, o a cobrar por él, es inaceptable por
razones comerciales, no técnicas. Ghostscript ya tuvo que salir por esto: es AGPL y
Artifex considera que una aplicación que lo invoca necesita licencia comercial.

## Decisión

1. Ninguna dependencia con copyleft fuerte (GPL, AGPL, LGPL, SSPL) en lo que se
   distribuye.
2. Permisivas sí: MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode-3.0, Unlicense, CC0,
   0BSD, BSL-1.0, NCSA.
3. **MPL-2.0 se permite**, con una salvedad explícita: es copyleft a nivel de
   archivo. Obliga a publicar cambios en *esos* archivos, nunca en el código propio,
   así que convivir con ella en un producto cerrado es posible mientras no se
   parchee. Entra por Tauri/wry (la pila de parsing CSS de Servo: `cssparser`,
   `selectors`, `dtoa-short`, `cssparser-macros`) y por `dwrote`, que no tiene
   alternativa permisiva en Windows.
4. La regla se verifica con `cargo deny`, no con buena voluntad: `deny.toml` se
   ejecuta en CI y en cada release.

## Consecuencias

- `pdfium` (BSD-3) sustituyó a Ghostscript como rasterizador, y `pdfium-render`
  (MIT/Apache) como binding. pdfium se fija por release (`chromium/7881`) para que
  las mediciones no deriven entre versiones.
- `cargo deny` bloquea el build si aparece una licencia fuera de la lista. En la
  primera pasada real encontró dos cosas: la pila MPL de Tauri (resuelta arriba,
  documentada) y **una vulnerabilidad** en `rustls-webpki 0.103.15`
  (RUSTSEC-2026-0382, acepta intermedios en los que la clave privada es pública),
  que se corrigió subiendo a 0.103.16.
- Los avisos de crates sin mantenimiento se ignoran por ID y con razón escrita, no
  en bloque: si un ID desaparece al actualizar, se borra la entrada.

## Alternativas descartadas

- **MuPDF** como rasterizador: AGPL, mismo problema que Ghostscript.
- **Excepciones por nombre de crate para MPL**: fue el primer intento. Empezó con
  tres crates y, al recorrer el árbol completo de Tauri, aparecieron cinco más
  (`cssparser-macros`, `dtoa-short`, `dwrote`, `libfuzzer-sys`…). Una lista que hay
  que perseguir en cada actualización se queda vieja y acaba permitiendo lo que no
  debe; se documentó la clase de licencia y su límite en su lugar.
- **Ignorar avisos de RustSec en general**: convierte el gate en decoración.

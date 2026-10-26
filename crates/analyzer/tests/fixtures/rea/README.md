# Corpus de referencia — CyberPlanet REA

Contenido copiado del estudio `cyberplanet-color-rea` (2026-10-22) para que los
tests de paridad sean autocontenidos.

| Ruta | Qué es |
|---|---|
| `render/*.png` | 35 páginas renderizadas a 150 dpi por el estudio (PdfCraft). Referencia externa de la medición. |
| `render_truth.json` | Las 11 métricas + clase por página, calculadas por `tools/render_truth.js` sobre esos PNG. |
| `pdfs/*.pdf` | Los 30 PDF deterministas del corpus, **reparados** (ver abajo). |
| `manifest.json` | Manifiesto original del estudio (sha256, tamaños, caso). |

## Dos defectos del corpus original

1. **Catálogo roto.** El generador (`gen_corpus.js`) emitió
   `/Type /Catalog /Pages 2 0 R`, pero el objeto 2 es `<< >>`; el árbol de
   páginas real está más adelante. Además cada `/Type /Page` lleva
   `/Parent 2 0 R`.
   PdfCraft lo toleró y renderizó el contenido; **pdfium y cualquier lector
   conforme sintetizan una página en blanco**. Los PDF de `pdfs/` son copias
   reparadas por `scripts/repair_rea_corpus.py`, que reapunta el catálogo y los
   `/Parent` al árbol real y reconstruye el xref sin tocar ningún otro byte.
   Los originales quedan intactos en la carpeta del estudio.

2. **Campo `page` poco fiable.** En `render_truth.json`, todas las páginas de un
   caso multipágina están registradas como `page: 1` (CP-017 tiene 6). El número
   real solo sobrevive en el nombre del PNG (`..._p3_150dpi.png`). Los tests lo
   derivan de ahí.

Ninguno de los dos afecta a MyPaper en producción: son defectos del generador
del corpus de investigación, no del pipeline de impresión.

## Cómo correrlo

```bash
# paridad pura de la medición (comparte píxeles, tolerancia 0.05 pp)
cargo test -p mpa-analyzer --test parity_rea

# end-to-end con pdfium (requiere la librería)
MPA_PDFIUM=/ruta/libpdfium.so \
  cargo test -p mpa-analyzer --features pdfium --test pdf_render
```

En Windows la DLL se busca en `%MPA_PDFIUM%`, junto a `mpa.exe`, y en el PATH,
en ese orden.
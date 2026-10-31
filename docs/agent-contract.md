# Contrato agente ↔ webapp

Dos repos, un contrato. No hay código compartido ni submódulo: el agente habla
HTTP con la webapp y, en el sentido contrario, con el navegador por loopback.

## Superficies

| Superficie | Quién inicia | Transporte | Versión hoy |
| --- | --- | --- | --- |
| Servidor → agente | agente | HTTPS `https://<dominio>/api/print-agent/v1/*` | **v1** (alias `/api/agent/*`) |
| Navegador → agente | pestaña del POS | HTTP `http://127.0.0.1:8632/v1/*` | **v1** (ya versionado) |

`crates/local-api` ya expone `/v1/status`, `/v1/handshake`, `/v1/jobs`,
`/v1/page.png` con token `x-mpa-local-token` emitido por el handshake. Ese lado
está resuelto.

El lado del servidor es el que necesita versión: hoy son 12 rutas bajo
`/api/agent/` (`pair`, `pairing-code`, `entitlement`, `tiers`, `tiers/[id]`,
`open-tickets`, `jobs`, `jobs/[id]/materialize`, `quote`, `local-grant`,
`heartbeat`, `version`).

## Forma implementada: `/api/print-agent/v1/*`

1. La webapp sirve `/api/print-agent/v1/*` y deja `/api/agent/*` como alias
   durante una ventana de deprecación: cada archivo del alias reexporta el
   handler canónico, sin lógica duplicada.
2. El agente manda dos cabeceras en cada request:
   - `X-Agent-Contract: 1`
   - `X-Agent-Version: <semver del binario>`
3. El servidor responde `426 Upgrade Required` si el contrato es menor al mínimo
   soportado, con JSON `{ "error": "contract_unsupported", "min": 1, "server": 1 }`.
4. Handshake al arrancar: `GET /api/print-agent/v1/capabilities` devuelve

```json
{
  "contract": 1,
  "server": "2.3.0",
  "min_agent": "0.4.0",
  "features": ["tiers", "quote", "materialize", "local-grant"],
  "limits": { "max_pages": 200, "max_bytes": 26214400 }
}
```

El agente cachea la respuesta y decide qué comandos de UI mostrar (por ejemplo,
ocultar "cotizar" si el servidor no lista `quote`).

5. Regla de compatibilidad: dentro de `v1` sólo se agregan campos opcionales y
   rutas. Renombrar o quitar campos exige `v2`.

## Tipos TypeScript desde Rust

`crates/sync` ya define los tipos del contrato (`Pairing`, `EntitlementToken`,
`RemoteTier`, `RemoteJob`, `Quote`, `QuotePage`, `QuoteLine`, `MaterializeAck`,
`VersionInfo`).

Recomendación: **ts-rs** sobre `specta`.

- ts-rs deriva un archivo `.ts` por tipo, con `serde` como fuente de verdad; no
  arrastra runtime al binario.
- specta está pensado para Tauri IPC (comandos), no para un contrato HTTP entre
  dos repos.

Cómo se usa:

```rust
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(export, export_to = "../bindings/"))]
pub struct RemoteTier { /* ... */ }
```

```powershell
cargo test -p mpa-sync --features ts export_bindings   # escribe crates/sync/bindings/*.ts
```

Los `.ts` viven en `crates/sync/bindings/` (incluye `PrintMode` exportado desde
`crates/domain` y `serde_json/JsonValue.ts`). Van commiteados: son el contrato
legible por el webapp y el diff de un PR muestra exactamente qué cambió en el
wire. `mpa-domain` y `mpa-sync` sólo pagan la dependencia con `--features ts`; el
binario del agente no la lleva.

En la webapp se importan desde `src/lib/types/agent/` (copia versionada por
commit, no `npm link`) y el CI falla si los `.ts` generados no coinciden con los
commiteados. Así el contrato no se rompe en silencio.

## Por qué dos repos

- Ciclos distintos: el agente se compila y firma para Windows; la webapp se
  despliega en Vercel/Supabase.
- Permisos distintos: el agente pasa por auditoría de instalador y antivirus; la
  webapp por revisión de producto.
- Contrato explícito: si todo vive junto, los tipos compartidos tientan a saltarse
  el versionado y el agente en la papelería —que no se actualiza solo— se rompe.

## Cuándo sí un monorepo

- Cuando hay más de un cliente del mismo contrato (agente Windows, agente Linux,
  app móvil) y el costo de sincronizar tres repos supera el de un workspace.
- Cuando se necesita un cambio atómico servidor + agente en el mismo PR y la
  ventana de deprecación no es viable.

Si llega ese día, el monorepo debe mantener **igual** el contrato versionado: el
versionado no es consecuencia del layout de repos, es una decisión aparte.

## Pendiente de aprobación

- Retirar el alias `/api/agent/*` cuando ningún equipo reporte
  `X-Agent-Version` menor a 0.4.0 en los logs.
- Paso de CI que compare los `.ts` de `crates/sync/bindings/` con los del webapp.

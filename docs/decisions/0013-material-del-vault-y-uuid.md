# ADR 0013 — UUID del cliente en el servidor y entrega de material a devices autenticados

- **Fecha:** 2026-10-16
- **Estado:** aceptado
- **Origen:** hueco encontrado al empezar el plugin (fase 3): sin esto, un segundo device (o el
  mismo tras reinstalar) **no puede desbloquear el vault jamás**.

## Contexto

El `vault_id` de 16 B (UUIDv7, generado por el cliente **antes de tocar la red**) entra en tres
bytes canónicos: el AAD de `wrapped_key_pw`, el AAD de `wrapped_key_rc` y `msg_bind` (→ `t_mac`).
La `sig` (`msg_rec`) **no** lo lleva, por eso el servidor podía verificar firmas sin conocerlo —
pero eso ocultó el hueco: **nada lo persistía**. Y `GET /vaults` devuelve `{id, name, role, rev}`:
sin salt, sin `kdf_params`, sin envueltas, sin UUID.

Consecuencia real: un device nuevo puede autenticarse (signin) pero **no puede derivar el KEK ni
unwrappear la `vault_key`** — el unwrap necesita `salt` + `kdf_params` + `wrapped_key_pw` + el
UUID (AAD). El material existe en la base (lo escribe `POST /vaults`) pero no había forma de
bajarlo. Ni `implementation-map.md` (que ya daba por contrato el `vault_id` en el create, con
"409 si el id ya existe") ni el protocolo definían la entrega.

## Decisión

1. **`POST /vaults` acepta `vault_id`** (b64, 16 B, **opcional** por retrocompatibilidad), lo
   persiste en `vaults.vault_uuid` (índice **único parcial**: NULL permitido para filas viejas)
   y responde **409** si ya existe (cuerpo `{"code":"bad_request"}`: la lista de códigos es
   cerrada y no tiene code de conflicto — el 409 es el status). El alta es atómica
   (`INSERT … ON CONFLICT DO NOTHING RETURNING id`).
2. **`GET /api/v1/vaults/{id}/material`** devuelve el material **público** del vault al dueño
   autenticado (misma resolución que todo lo demás: 404 si no es suyo):

   | campo | forma | nota |
   |---|---|---|
   | `id` | número | PK interna del servidor — **jamás** entra en bytes derivados |
   | `vault_id` | b64 16 B o `null` | el UUID del cliente (AADs) |
   | `name` | texto | — |
   | `enc_version` / `epoch` | número | el epoch es campo firmado |
   | `salt` | b64 16 B | — |
   | `kdf_alg` / `kdf_params` | texto | `kdf_params` va **tal cual se guardó** (JSON en texto, byte-fiel: el cliente lo parsea, el server no lo re-serializa) |
   | `wrapped_key_pw` / `wrapped_key_rc` | b64 | ciphertext bajo el KEK/recovery: sin password/recovery son inútiles |
   | `vault_pubkey` | b64 32 B | Ed25519: el server **verifica** con esto, no guarda privadas |

3. **Nada de esto es secreto**: envueltas y pubkey son datos que el diseño asume en el server;
   la frontera sigue siendo la password (y la recovery), que **nunca** viajan al servidor.

## Consecuencias

- El device 2 (y el CLI `setup` de otra máquina, y el plugin tras reinstalar) tienen un camino:
  signin → `GET material` → derivar KEK con la password → unwrappear → proof de posesión.
- Filas creadas antes de este ADR quedan con `vault_uuid = null`: desbloqueables solo donde el
  material local exista (el camino viejo). No hay backfill posible (el UUID no se guardó), y no
  se inventa uno: los AADs ya están fijados.
- La verificación de firmas del server **no cambia** (`msg_rec` no lleva el UUID).

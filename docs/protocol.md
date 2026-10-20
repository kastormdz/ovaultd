# Protocolo `sync2` (vigente)

> **Estado:** vigente desde 2026-09-25. Sustituye al protocolo v1 (ver ADR-0003, ADR-0004 y ADR-0005).
> Un cambio acá es un ADR nuevo, no un edit silencioso. Backup de v1: `protocol.v1.bak.md`.

## Principios

1. Dos planos separados: HTTP/JSON para control (auth, vaults, historia) y WebSocket para datos.
2. Revision monotona por vault como unica unidad de orden. El cliente dice `since` y recibe lo posterior.
   `changes` pagina por revision completa (nunca corta un batch a la mitad) y devuelve cursor.
3. Idempotencia por `operation_id`. Subir dos veces la misma operacion devuelve el mismo `ack`.
   No se usa `(path_id, hash)` como clave: eso se traga un cambio real (restaurar un borrado con el mismo contenido).
4. Fencing + compare-and-swap. El cliente no sube la siguiente tanda hasta el `ack`, y cada `batch` lleva **un**
   `expected_rev` para toda la tanda. Si la revision cambio, `conflict`. Sin precondicion, el `ack` no protege nada: el ultimo gana en silencio.
5. El server es ciego: no ve contenido, ni paths, ni estructura. Ve `path_id`, `blob_id`, tamanos, revisiones,
   dispositivo e igualdad de paths y contenido (ver `crypto.md`).
6. El modo de sync y la resolucion de conflictos los decide el cliente.
7. Todo registro va firmado (Ed25519) y el cliente rechaza cualquier historia que retroceda.

## Control (HTTP/JSON, `Authorization: Bearer <token>`)

| Metodo | Ruta | Cuerpo / respuesta |
|---|---|---|
| POST | `/api/v1/auth/signin` | `{email, password, mfa?}` -> `{token, device_id}`. La password es de cuenta y el server la guarda hasheada (argon2id): es independiente de la password E2E (ADR-0011). `mfa` presente -> `not_implemented` mientras no exista: no se ignora en silencio |
| POST | `/api/v1/devices` | `{name, platform}` con token de un device de la MISMA cuenta -> `{token, device_id}`. Enrolar un device es una operacion autenticada, no un registro publico |
| POST | `/api/v1/auth/signout` | - |
| GET | `/api/v1/vaults/{id}/challenge` | `{challenge}` - 32 B efimeros, anti-replay |
| GET | `/api/v1/me` | cuenta y uso de storage (sin concepto de plan) |
| GET | `/api/v1/vaults` | vaults propios + compartidos |
| POST | `/api/v1/vaults` | `{name, enc_version, salt, kdf_alg, kdf_params, wrapped_pw, wrapped_rc, vault_pubkey}` -> `{id}` |

> El challenge va por vault (`/vaults/{id}/challenge`) y no por cuenta: `auth_challenges` lo ata a
> `(device_id, vault_id)`, asi un challenge emitido para un vault no se puede reusar en otro. El `proof` se
> verifica con `vaults.vault_pubkey` y el challenge se consume (uso unico) despues de verificar: si dos
> pedidos concurrentes verifican el mismo challenge, solo uno gana el consumo y el otro recibe 401.

| Metodo | Ruta | Cuerpo / respuesta |
|---|---|---|
| POST | `/api/v1/vaults/{id}/access` | `{proof}` (firma del challenge) -> `{host, rev}` - 403 si no valida |
| GET | `/api/v1/vaults/{id}/changes?since=&limit=N` | delta por revision completa: `{items:[...], next_since, min_rev?}` |
| GET | `/api/v1/vaults/{id}/tree` | estado actual (reconciliacion completa on-open) |
| GET | `/api/v1/vaults/{id}/history?path_id=&limit=` | revisiones de un archivo (incluye `path_enc` y `prev_path_id`) |
| POST | `/api/v1/vaults/{id}/restore` | `{path_id, rev}` o `{snapshot_id}` -> crea revision nueva |
| POST | `/api/v1/vaults/{id}/snapshots` | `{label?}` -> `{id, rev, manifest}` |
| GET / DELETE | `/api/v1/devices[/{id}]` | listar / revocar |
| POST / DELETE | `/api/v1/vaults/{id}/members[/{account}]` | invitar / retirar colaborador (fase 8) |
| POST | `/api/v1/vaults/{id}/leave` | abandonar un vault compartido (no es desvincular un device) |
| GET / PUT | `/api/v1/vaults/{id}/config` | modo, filtros, estrategia de conflicto |
| POST | `/api/v1/vaults/{id}/blobs` | crea subida reanudable (`tus` core: `Upload-Length`/`Upload-Metadata`); `PATCH`/`HEAD` en `/vaults/{id}/uploads/{upload_id}` |
| GET | `/api/v1/vaults/{id}/blobs/{hash}` | descarga del CAS (`Range`) - scoped al vault y autorizada por ACL |

`op` in `{add, mod, del, move}`. Un restore se expresa como `add` (path inexistente) o `mod` (path existente).
`min_rev` presente => el cliente quedo fuera de la ventana de retencion => resync completo.

## Datos (WebSocket `/api/v1/ws`)

El upgrade se autentica con `Authorization: Bearer <token de device>` o, para clientes que
no pueden fijar headers en un WebSocket (navegadores: el runtime del núcleo y el plugin corren
en Chromium/Electron), con el subprotocolo `ovaultd.bearer.<token en base64url sin padding>`
acompañado de `ovaultd.v1`, que el server ecoa. El bearer nunca se ecoa. Ver ADR-0015.

### Handshake (se prueba la clave, no se la manda)

```json
// C->S
{"t":"hello","proto":1,"vault":12,"device":7,"since":1834,"modes":{"sync":"bidirectional","conflict":"merge"}}
// S->C
{"t":"hello_ok","proto":1,"challenge":"<b64 - 32 B aleatorios, efimeros>","head_rev":1850}
// C->S
{"t":"auth","proof":"<b64 - Ed25519_sign(k_sign, challenge | vault_id | device_id)>"}
// S->C
{"t":"auth_ok","rev":1834}
```

El server valida con la clave publica del vault: no guarda ningun secreto capaz de forjar un proof.
`proto` negocia el protocolo de la app (no es un mecanismo de seguridad).

### Operaciones

```json
// C->S
{"t":"batch","batch_id":"<string uuid>","operation_id":"<uuid hex: 16 B, guiones opcionales>","expected_rev":1834,
 "ops":[
   {"op":"mod","path_id":"<b64>","path_enc":"<b64>","folder":false,"blob_id":"<b64>","t_mac":"<b64 firma>",
    "sig":"<b64 firma>","size":12043,"mtime":1758831000000,"ctime":1758800000000,"prev_path_id":null},
   {"op":"move","path_id":"<b64 nuevo>","path_enc":"<b64 nuevo>","folder":false,"prev_path_id":"<b64 viejo>",
    "blob_id":"<b64>","t_mac":"<b64 firma>","sig":"<b64 firma>","size":12043,"mtime":1758831000000,"ctime":1758800000000},
   {"op":"del","path_id":"<b64>","path_enc":null,"folder":false,"blob_id":null,"t_mac":null,"sig":"<b64 firma>",
    "size":null,"mtime":null,"ctime":null,"prev_path_id":null}
 ]}
{"t":"batch_ack","batch_id":"<string uuid>","rev":1835}
{"t":"pull_ack","rev":1835,"ok":true}
{"t":"ping"}
// S->C
{"t":"notify","rev":1835,"op":"mod","path_id":"<b64>","blob_id":"<b64>","t_mac":"<b64 firma>","sig":"<b64 firma del registro>",
 "size":12043,"device":"laptop (Linux)","deleted":false,"prev_path_id":null}
{"t":"blob_head","rev":1835,"transfer":"a91","blob_id":"<b64 32 B = hash del contenido>","len":12043,"offset":0}
{"t":"ack","rev":1834}
{"t":"conflict","path_id":"<b64>","base_rev":1200,"server_rev":1835,"client_rev":1831}
{"t":"error","code":"max_file_exceeded","detail":"limite del vault: 12 MB > 10 MB configurado"}
```

Reglas:

- Un batch = UNA revision. Las N operaciones entran en una sola transaccion y comparten `rev`: `rename + edit`
  no se parte en dos revisiones (una unidad deshacible). `changes` sigue paginando por revision completa: un batch
  entra entero o no entra.
- CAS obligatorio: `expected_rev` (uno por batch, no por operacion) se valida en la misma transaccion que
  asigna la revision. Si no coincide -> `conflict` con el `path_id` real de la operacion que perdio la carrera (los
  `ops` de un batch pueden ser varios: "que archivo" no se deduce del mensaje).
- Campos por operacion: `path_enc` (nombre cifrado, `null` solo en `del`) y `folder` (explicito, nunca
  derivado de la ausencia de `blob_id`: una carpeta y un tombstone se parecen demasiado). `sig` va en todas,
  incluidas las `del`. Ver la tabla completa en ADR-0008.
- Los blobs van primero: el servidor no acepta un batch que referencie un blob inexistente (`not_found`). El CAS
  de contenido es un filesystem, no participa de la transaccion; primero se publica el blob, despues se referencia.
- Tope de operaciones: `MAX_BATCH_OPS = 1000` por batch -> `bad_request` si se supera. Los `ops` son metadata
  (~300 B cada uno): 1000 ops ~ 300 KB.
- Registros firmados: el cliente valida `sig` y rechaza un `rev` menor al ultimo firmado que vio (anti-rollback).
- Idempotencia: repetir el mismo `operation_id` devuelve el mismo `ack` sin crear revision.

### Transporte binario: un solo camino por blob (ADR-0004)

| Tamano | Camino |
|---|---|
| <= umbral (default 1 MB) | inline en frames binarios WS: `blob_head` -> N frames, prefijo `transfer_id` (8 B) + `offset` (8 B) |
| > umbral | siempre HTTP reanudable: `POST /vaults/{id}/blobs` (`Content-Range`) |

`transfer` identifica una subida de bytes, no una operacion de metadata: por eso vive en este plano y no en el
`batch`. Nunca los dos caminos para el mismo blob. El `blob_id` viaja en el `blob_head` para que el receptor pueda
verificar que los bytes hashean a lo que el emisor dice: misma regla que el PUT por HTTP, donde un hash que no
coincide no se publica (400) y no deja nada en el CAS. El receptor mantiene una cola de frames (un frame que llega antes de que el
handler este listo se bufferea, no se descarta - bug real del cliente oficial, corregido en su v0.0.3), y la
reanudacion usa el offset explicito del prefijo (el oficial reenvia el archivo entero).

## Algoritmo del cliente

```
1. index      escanear el vault -> {path_id, content_hash, size, mtime, ctime, folder}
2. encolar    registrar los cambios LOCALES primero (content_hash/mtime), antes de tocar nada remoto
3. hello      WS con `since` = ultima revision vista
4. notify*    aplicar el stream del server en orden de rev
              - validar `sig` y descartar historia que retroceda (anti-rollback)
              - 'del'  -> tombstone local + archivo al .trash (borrado efectivo, nunca silencioso)
5. decision por path CONTRA LA BASE sincronizada (no contra "ambos existen"):
   local==base y remoto==base            -> nada
   local!=base  y remoto==base           -> batch (paso 7)
   remoto!=base y local==base            -> pull
   ambos!=base y distintos entre si      -> CONFLICTO (paso 6)
   ambos!=base pero iguales entre si     -> convergencia, sin conflicto
6. conflicto
   hay base comun -> merge 3-vias char-level / copia de conflicto (binario)
   no hay base    -> copia con timestamp, gana el server en el original
7. batch      carpetas primero, luego archivos por TAMANO ASCENDENTE; un `expected_rev` + un `operation_id`
              para toda la tanda (una sola revision al final)
8. batch_ack  esperar el `ack` (con su `rev`) antes de la siguiente tanda
9. repetir hasta que una pasada no haga nada
```

## Merge (3 vias, char-level)

No alcanza con "usar diff-match-patch": hay que definir la composicion.

- `d_ours = diff(base, ours)`, `d_theirs = diff(base, theirs)`; proyectar ambos sobre la base.
- Rangos de cambio sin solapamiento -> aplicar los dos (merge automatico).
- Solapamiento o inserciones concurrentes en el mismo punto -> conflicto.
- Implementacion: `patch_make(base, ours)` aplicado sobre `theirs` con `match_threshold` explicito; cada parche
  rechazado es un conflicto, nunca un exito parcial.
- `merge` -> markers visibles para revision; `conflict` -> copia `nombre (conflicted copy <fecha>).md`.
- Binarios/canvas/base: nunca merge, copia de conflicto.
- Toda resolucion es una revision nueva (restaurable).

## Reintentos y backoff

- Por archivo: `2^n * 5 s`, cap 5 min. Un path con backoff saltea a sus hijos.
- Conexion: backoff exponencial con jitter; `success()` resetea el contador.
- Polling de rescate: cada 30 s (no como mecanismo principal).

## Renames, tombstones y guardas

- Rename: `blob_id` igual + `path_id` nuevo + `prev_path_id` => move (solo metadata, se recalcula `T`).
- Delete: tombstone (`deleted = true`) con retencion (default 30 dias). Nunca borra blobs de inmediato.
  El cliente retira el archivo visible y lo manda al trash: un tombstone que deja el archivo a la vista no borra nada.
- Guarda anti-desastre (obligatoria): cliente recien inicializado o indice vacio + remoto con datos => prohibido
  propagar borrados. Requiere confirmacion explicita ("adoptar remoto" / "sobreescribir remoto").
- Guardas de borrado: un archivo local ausente con blob remoto intacto y sin tombstone NO es un borrado
  (cubre el caso "on-demand / online-only" que en el servicio oficial borra el remoto). Requiere confirmacion.
- Regla de oro: un solo sistema de sync por vault.

## GC y backups

- Referencias vivas (tres fuentes): `files.blob_hash`, `revisions.blob_hash` (dentro de la retencion) y
  `snapshots.manifest`. Las tres vacias => blob huerfano.
- Publicacion de blobs en dos fases: temporal -> `fsync` -> mover al CAS -> recien ahi referenciar en la transaccion.
  El filesystem/S3 no participa de la transaccion SQL.
- Gracia + exclusion: nunca se borran blobs con menos de N horas; `gc --verify` corre en modo exclusivo; `--dry-run` es default.
- `refs` (si se guarda) se mantiene en la misma transaccion; alternativa valida: derivarlo en la consulta.
- Backups: dump de la base + snapshot del CAS con escritura quiescida => par consistente. Restore verificado.

## Cuotas y tamano de contenido

- `quota_bytes`, opt-in: sin limite por defecto en self-hosted (`NULL` = ilimitado). Es politica, no seguridad.
- `max_file_bytes`, con techo duro: el contenido tiene un maximo de protocolo `MAX_FILE_BYTES = 32 MiB`
  (`33554432`), porque el contenido se cifra con un AEAD de mensaje unico y el costo de la pasada 2 es O(~4n)
  (medido: `docs/crypto-gate.md` H-3; decidido en ADR-0007). Por vault se puede bajar el techo,
  nunca superarlo; `NULL` = usar el techo del protocolo. El cuerpo subido (blob almacenado) es el contenido
  mas el sobre `nonce[24] || tag[16]` (ADR-0014): el tope del cuerpo es este valor + 40 B.
- Sin concepto de `plan` ni tiers comerciales.
- Enforcement en la subida (no despues), antes de asignar la revision, y con error explicito sin corromper
  estado: tamano declarado o real > tope => `max_file_exceeded` incluyendo el limite vigente en `detail`.
- Un archivo que supera el tope se RECHAZA; nunca se saltea en silencio. Un salteo silencioso es indistinguible
  de una perdida de datos (y rompe la garantia de que "sincronizado" significa sincronizado).
- El camino reanudable reserva la cuota del tamano declarado al crear la subida (el `total` de toda subida viva);
  vencida la subida (TTL default 24 h, `OVAULTD_UPLOAD_TTL_HOURS`) la reserva se libera. Dos subidas simultaneas no pueden
  exceder la cuota: la segunda ve la reserva de la primera.

## Errores

Un solo formato: `{"t":"error","code":"<snake_case>","detail":"<texto>"}`. El `code` no lleva prefijo `t:`: el
prefijo `t:` es de los mensajes (`t:batch`, `t:conflict`), no de los codigos de error.

Lista cerrada (ampliarla requiere un ADR, para que cliente y servidor no se desincronicen):

| `code` | Cuando |
|---|---|
| `unauthorized` | dispositivo revocado, firma de auth invalida, ACL ajena |
| `not_found` | vault, revision o blob inexistente |
| `bad_request` | mensaje mal formado o campo faltante (nunca por contenido cifrado) |
| `max_file_exceeded` | el contenido supera el tope (ADR-0007); `detail` incluye el limite vigente |
| `quota_exceeded` | la cuota opt-in del vault se agotaria con esta subida |
| `resync_required` | `since < min_rev`: hay que resincronizar completo (nunca una delta silenciosa) |
| `internal` | fallo del servidor; el cliente reintenta con backoff |

> Conflicto NO es un error: el CAS fallido devuelve el mensaje `t:conflict` (`base_rev`/`server_rev`/`client_rev`),
> porque es un estado normal del algoritmo, no una falla.

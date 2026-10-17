# ADR-0008 — Sobre del batch: cómo N operaciones forman **una** revisión

- **Fecha:** 2026-09-25
- **Estado:** **ACEPTADO** (2026-09-25). Decisión del usuario: *"Hace el batch entero"* → **Opción A**.
- **Cierra:** **G-8** (hueco detectado al escribir `crates/ovaultd-protocol`)

## Contexto: el hueco

Tres piezas del proyecto exigen que un grupo de cambios sea **una sola revisión atómica**:

- `docs/build-plan.md`: «Un `batch_id` produce una revisión con todas sus filas. `rename + edit` no se parte en
  commits independientes».
- `migrations/001_init.sql`: la tabla `batches` agrupa los cambios atómicos de una revisión; `revisions` tiene PK
  `(vault_id, rev, path_id)` — varias filas pueden compartir `rev`.
- `docs/protocol.md`: «`changes` pagina **por revisión completa** (nunca corta un batch a la mitad)».

Pero el protocolo define el `push` con **una sola** operación (`op`, `path_id`, `blob_id`, `t_mac`, `sig`) y un
`expected_rev` por mensaje. **Nada dice cómo llegan N operaciones en una misma revisión.** El algoritmo del cliente
habla de "tandas" pero sin forma de mensaje. Es un hueco real, no un detalle: si se implementa como N `push`
independientes, un `rename + edit` se parte en dos revisiones y **la garantía de atomicidad desaparece** (y con ella la
posibilidad de deshacer un rename+edit como una unidad).

## Opciones

### A. Un mensaje `batch` con la lista de operaciones — **recomendada**

```json
{"t":"batch","batch_id":"<b64 16 B>","operation_id":"<b64 16 B>","expected_rev":1834,
 "ops":[
   {"op":"move","path_id":"<b64>","prev_path_id":"<b64>","blob_id":"<b64>","t_mac":"<b64>","sig":"<b64>",
    "size":12043,"mtime":1758831000000,"ctime":1758800000000},
   {"op":"mod","path_id":"<b64>","blob_id":"<b64>","t_mac":"<b64>","sig":"<b64>",
    "size":12100,"mtime":1758831009999,"ctime":1758831009999}
 ]}
```

El `ack` devuelve **una** `rev` para todo el batch.

- ✅ Atómico **por construcción**: una transacción, una revisión, un `operation_id`.
- ✅ No introduce estados intermedios que persistir ni limpiar.
- ✅ El `batches`/`batch_id` del esquema ya existen: encaja sin migración.
- ⚠️ El mensaje crece con la cantidad de operaciones. Los `ops` son **metadata** (~200–300 B cada uno): 1.000 ops ≈
  300 KB. Se pone un tope explícito y listo.

### B. `batch_begin` + N `push` + `batch_commit`

- ✅ Permite emitir operaciones en streaming, sin armar el array completo.
- ❌ Introduce un **estado intermedio persistido** ("ops sin revisión") en el servidor, con su propia limpieza,
  vencimiento y semántica de fallo: ¿qué pasa si el `batch_commit` nunca llega? Un servidor **ciego** no debería
  necesitar ese estado.
- ❌ Más round trips y más modos de error, para un caso (miles de ops por revisión) que en un vault de notas no ocurre.

## Decisión propuesta

**Opción A.** La atomicidad se consigue sin inventar estado persistido, que es exactamente lo que un servidor ciego
tiene que evitar. Si algún día una tanda es tan grande que el mensaje molesta, se puede pasar a B sin cambiar el
modelo de datos (el `batch_id` sigue agrupando igual).

Complementos que fija este ADR si se acepta:

| Punto | Decisión |
|---|---|
| `transfer` | **Sale del `push`** y se muda a los mensajes de **transporte de blobs**: identifica una subida de bytes, no una operación de metadata |
| Tope de ops | `MAX_BATCH_OPS = 1_000` (constante de protocolo) → error `bad_request` si se supera |
| Orden de subida | El **blob primero**, después el `batch` que lo referencia: el servidor no puede aceptar una revisión apuntando a un blob inexistente |
| `expected_rev` | **Uno por batch** (el `push` por operación desaparece como mensaje del wire) |
| Vectores cripto | **Sin cambios**: el batch es una decisión de wire, no de cripto. Los 32 vectores siguen válidos |
| `changes` | Sigue paginando por revisión completa: un batch entra entero o no entra |

## Forma final del `BatchOp` (aceptada)

```json
{"op":"mod","path_id":"<b64>","path_enc":"<b64>","folder":false,"blob_id":"<b64>","t_mac":"<b64>",
 "sig":"<b64>","size":12043,"mtime":1758831000000,"ctime":1758800000000,"prev_path_id":null}
```

| Campo | Regla |
|---|---|
| `op` | `add` \| `mod` \| `del` \| `move` |
| `path_id` | el path destino (en `move`, el **nuevo**) |
| `path_enc` | nombre cifrado; **`null` solo en `del`** (G-13) |
| `folder` | **explicito**, nunca derivado de la ausencia de `blob_id` (G-12) |
| `blob_id` | `null` en `del` y en carpetas |
| `t_mac` | binding path↔blob; `null` sin blob |
| `sig` | firma Ed25519 del registro: **toda op la lleva, incluidas `del`** |
| `size`/`mtime`/`ctime` | metadata; ausentes en `del` |
| `prev_path_id` | solo en `move`: el path viejo |

`batch_id` y `operation_id` son b64 de 16 B. `expected_rev` es **uno por batch**.

## Impacto si se acepta

- `docs/protocol.md`: reemplazar el ejemplo `push` por `batch`, agregar `MAX_BATCH_OPS` y la sección de transporte de
  blobs con `transfer`.
- `crates/ovaultd-protocol`: `Push` pasa a ser `Batch { batch_id, operation_id, expected_rev, ops: Vec<BatchOp> }`, y el
  test del wire se actualiza.
- `docs/implementation-map.md`: la fila `batch_id` pasa a ser el sobre del commit.
- **No** toca: cripto, vectores dorados, esquema, ni el algoritmo del cliente.

---

## G-12 — `BatchOp` necesita `folder` (destapado al escribir el contrato del commit)

El esquema tiene `files.is_folder NOT NULL DEFAULT 0` y el algoritmo del cliente manda **carpetas primero**. Pero
una carpeta y un **tombstone** se parecen: los dos van **sin `blob_id`**. Sin un campo explicito, el servidor ciego
no puede distinguir "crear esta carpeta" de "borrar este archivo" — y el `op` tampoco lo resuelve, porque `del` de
una carpeta es legitimo.

- **Decidido:** cada op lleva **`"folder": true\|false`** explicito (no derivado de `blob_id` ausente).
- **Por que no derivarlo:** un campo derivado que se puede deducir mal es una bomba de tiempo silenciosa; un booleano
  en el wire se puede testear. Costo: 1 campo.

## G-13 — El wire no llevaba `path_enc` (destapado al escribir el contrato del commit)

El `Push` del protocolo lleva `path_id`, `blob_id`, `t_mac`, `sig`, `size`, `mtime`, `ctime` y `prev_path_id`. **No lleva
`path_enc`.** Pero el esquema lo exige en `files` y en `revisions` con este comentario:

```sql
path_enc  BLOB,   -- necesario para historia y bootstrap (un device nuevo no deriva nombres de HMACs)
```

Y es literal: `path_id = HMAC(k_path, path)` es de un solo sentido, **de ahi no sale el nombre**. Si el cliente no manda
`path_enc` al subir, el servidor no lo tiene y **un dispositivo nuevo jamas puede bajar el arbol** (recibe HMACs sin
nombre). El agujero no se ve en ninguna prueba de cripto ni de wire: se ve cuando un segundo device intenta un resync
completo, o sea en produccion y con datos reales adentro.

- **Decidido:** cada op lleva **`path_enc`** (b64; `null` solo en un tombstone, que por definicion no tiene nombre que
  mostrar).
- **No se deriva en el servidor:** el servidor es ciego y no tiene `k_path`; guarda el nombre cifrado tal como llega.

## G-12 — `BatchOp` necesita `folder` (destapado al escribir el contrato del commit)

`protocol.md` dice "Si la revision cambio -> `conflict`", pero la única estructura `Conflict` del protocolo
(`path_id`, `base_rev`, `server_rev`, `client_rev`) es la del **merge de 3 vías**. El rechazo de un push quedaba sin forma.

- **Descartado:** un código de error. `conflict` no está entre los 7 códigos, y la primera versión de `error.rs`
  mapeaba el conflicto a `internal`: eso le dice al cliente "bug del servidor" cuando el conflicto es un resultado
  normal y esperado del compare-and-swap. Un cliente que no distingue "perdí la carrera" de "se rompió" no puede
  reintentar bien.
- **Decidido:** viaja como el **mensaje** `{"t":"conflict",...}` con HTTP 409 y **sin** código de error.
- **A ratificar con esta ADR:** `path_id` = el del push, `base_rev` = `expected_rev`, `server_rev` = `head_rev`,
  `client_rev` = `expected_rev` (el cliente no commiteó: su base sigue siendo la misma).
- **`path_id` viaja de verdad, nunca en ceros.** La primera implementacion mando `PathId::new([0; 32])` porque la
  variante de error no lo llevaba: un campo de 32 bytes en cero **parece válido** (el cliente no puede distinguir
  "no aplica" de "el archivo cero"), así que el conflicto llegaba sin decir **qué** archivo perdió la carrera. En un
  push de una op el cliente lo deduce; en un batch de N, no. La variante de error ahora lo lleva.
- **Por qué queda pegado a esta ADR:** el sobre del batch (opción A o B) cambia **qué significa** `expected_rev` en un
  push con N operaciones, así que la forma final se congela junto con el sobre. No antes.

### Ratificado con la aceptacion de este ADR

```json
{"t":"conflict","path_id":"<b64>","base_rev":1834,"server_rev":1850,"client_rev":1834}
```

- `path_id`: el de la **primera** op del batch. Es un `path_id` **real** de la tanda (nunca ceros) y viaja
documentado como *representativo*: el CAS es del **batch entero**, no de una operacion, asi que el cliente rebasa
  la tanda completa contra `server_rev`.
- `base_rev` = `expected_rev` del batch, `client_rev` = lo mismo (el cliente no commiteo: su base no cambio),
  `server_rev` = `head_rev` vigente.
- HTTP 409, **sin** codigo de error: un conflicto es un resultado normal del compare-and-swap, no una falla.

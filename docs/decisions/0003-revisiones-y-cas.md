# ADR 0003 — Revisiones multi-archivo, CAS e idempotencia real

- **Fecha:** 2026-09-25
- **Estado:** aceptado · sustituye a las reglas 3 y 4 del protocolo v1
- **Origen:** § 17.5 de la spec canónica (hallazgos 🔴 de la segunda revisión)

## Contexto

El protocolo v1 tenía tres defectos verificados contra su propio texto:

1. **Idempotencia por `(vault, path_id, hash)`.** Suena razonable y **se traga un cambio real**: si el archivo se
   borró y se restaura con el **mismo contenido**, el hash coincide y el server lo trata como no-op → el borrado
   nunca se deshace.
2. **El fencing no protegía nada.** El `ack` dice "hasta dónde llegó el server", no "nadie escribió desde que leíste".
   Sin precondición, dos dispositivos que editan desde la misma base convergen a **last-write-wins silencioso**.
3. **`batch_id` (agrupar N cambios en una revisión atómica) rompía la paginación.** Con varias filas por `rev`, un
   `changes?limit=` que cortaba la revisión a la mitad hacía avanzar `since` y **perdía eventos sin error**.

## Decisión

1. **`operation_id` (UUID) generado por el cliente** como clave de idempotencia. Repetirlo devuelve el mismo `ack` y
   no crea revisión. El estado (incluido `deleted`) se evalúa en la operación, no por hash.
2. **Compare-and-swap obligatorio:** cada `push` lleva `expected_rev`; se valida **en la misma transacción** que
   asigna la revisión. Si no coincide → `t:conflict` con `base_rev`/`server_rev`/`client_rev`, el cliente rebasa
   contra la base común y reintenta. Un `del` cuenta como `mod` a efectos de CAS.
3. **Atomicidad por revisión completa** (`batch_id`, PK `(vault_id, rev, path_id)`), y la paginación **nunca corta una
   revisión**: `changes` devuelve revisiones enteras, `next_since` como cursor y `min_rev` si el cliente quedó fuera
   de la ventana de retención (⇒ **resync completo**, no delta silencioso). Tamaño máximo de batch explícito.
4. **El detalle de una revisión no se confirma por hash sino por estado:** `(path_id, op, blob_id, t_mac, sig, epoch)`.

## Consecuencias

- El cliente necesita **una base** por path (última revisión sincronizada conocida) para rebasar; sin base, la
  divergencia se resuelve con copia de conflicto, no con CAS.
- `operation_id` debe persistirse del lado del cliente en la cola de push para sobrevivir a un crash.
- Un cliente offline demasiado tiempo no puede "recuperar la delta": paga un resync completo. Es la contrapartida
  honesta de no listar el vault entero para sincronizar.

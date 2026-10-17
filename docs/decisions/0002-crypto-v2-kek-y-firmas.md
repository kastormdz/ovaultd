# ADR 0002 — Cripto v2 (`sync2`): KEK, clave de datos envuelta y binding por firma

- **Fecha:** 2026-09-25
- **Estado:** aceptado · **sustituye** a la spec `sync1` (`docs/crypto.md` v1, archivo `crypto.v1.bak.md`)
- **Origen:** segunda revisión externa (§ 17.5 de la spec canónica) sobre la spec ya corregida una vez

## Contexto

La primera revisión externa ya había cambiado el cifrado determinista por un nonce derivado de una clave
(*MLE keyed*) más un MAC de binding `T`. La segunda revisión encontró que **esa corrección seguía teniendo agujeros
propios** — y tres de ellos eran de arquitectura, no de detalle:

1. **El binding no ataba el nombre.** `T` unía `path_id` ↔ `blob_id`, pero `path_enc` viajaba sin atar a su
   `path_id`: un server malicioso podía **cambiar el nombre cifrado** y el cliente mostraba otro nombre para el mismo
   contenido **sin invalidar `T`**.
2. **Borraba y reproducía impunemente.** Los `del` no estaban autenticados (se podían ocultar) y nada impedía
   **re-servir un par histórico válido** como si fuera nuevo (rollback).
3. **El proof de clave era falsificable.** Con `HMAC(k_keyhash, challenge)` el server guardaba material con el que
   **podía forjar proofs**, y la afirmación "nunca viaja crudo" era falsa.

Además, con la clave de datos derivada directo de la password, **rotar la password cambiaba `path_id`** y obligaba a
migrar el índice entero: la rotación era una operación que nadie iba a hacer nunca.

## Decisión

1. **KEK + clave de datos aleatoria envuelta.** `kek = Argon2id(password, salt, m=64 MiB, t=3, p=4)`; `vault_key` =
   256 bits **aleatorios**, guardada envuelta (`wrapped_pw`, `wrapped_rc`). Rotar la password = re-envolver un registro;
   `path_id`, `blob_id` y `T` no cambian. Subir `epoch` (re-cifrado real) queda como operación explícita y sospechosa.
2. **Identidad Ed25519 derivada del vault.** El server guarda **sólo la clave pública**. El handshake es
   challenge-response firmado y **cada registro del oplog va firmado**: eso autentica también los borrados y habilita
   el **rechazo de historia que retroceda** (anti-rollback) en el cliente.
3. **`path_enc` con nonce aleatorio y `aad = path_id`.** Cierra el agujero 1 sin volver al cifrado determinista.
4. **Binding en metadata firmada, no en el AAD del cuerpo.** `T` = firma sobre `vault_id § path_id § blob_id § epoch`.
   Mantiene dedupe y hace el rename barato (32 B de metadata), cosa que `aad = path_id` en el contenido **rompía**.
5. **`k_nonce` separada de `k_content`** para el nonce determinista *keyed*; `blob_id` = SHA-256 del **ciphertext**.
6. **Argon2id versionado por vault** (`kdf_alg`, `kdf_params`) en lugar de scrypt `N=32768`.
7. **Recovery con entropía declarada:** secreto aleatorio de 256 b + base32 con checksum, envuelto sobre `vault_key`.

## Consecuencias

- **Los vectores de prueba `sync2` no existen todavía**: el spike Rust↔JS probó **`sync1`**. Re-ejecutarlo con los
  parámetros nuevos es **bloqueante de la fase 1** (tarea 0.2 del plan). La interop sigue importando porque el CLI
  (Rust) y el plugin (JS) deben derivar las mismas claves.
- Se **conservan** los vectores `sync1` como referencia histórica y por el pitfall de NFKC.
- Fugas documentadas como decisión, no como accidente: igualdad/estabilidad de `path_id`, dedupe dentro del vault,
  **dedupe oracle entre miembros** de un vault compartido, y tamaños/mtimes/dispositivo.
- **Alternativa legítima no elegida:** nonce aleatorio por blob y sin dedupe (más confidencialidad, menos dedupe).
  Cambiarla es un ADR nuevo, no un ajuste.
- Invalidar una recovery **no** revoca `wrapped_rc` que alguien haya copiado antes.

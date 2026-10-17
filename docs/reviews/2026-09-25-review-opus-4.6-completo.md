# Revisión crítica — `obsidian-sync-selfhost-spec.md`

> **Revisor:** Senior técnico · **Fecha:** 2026-09-25 · **Versión del doc:** propuesta inicial

---

## 🔴 Rompe algo

### 1. `path_enc` determinista filtra igualdad de paths entre versiones (§ 5, § 3)

§ 5 dice `path_enc` usa "AEAD con nonce derivado de `path_id` (determinista, sin filtrar)". Un cifrado determinista **por definición filtra igualdad**: un atacante con acceso a la DB ve cuándo dos revisiones tocan el mismo path, y puede confirmar si un path conocido existe comparando ciphertexts. Esto contradice la promesa de § 2 ("el servidor es **ciego**") y el RNF-05. El `path_id` (HMAC) ya cumple la función de índice ciego; `path_enc` debería usar nonce aleatorio (el cliente lo descifra con `k_path`, no necesita determinismo) o eliminarse si el cliente reconstruye el path desde su estado local + `path_id`.

### 2. AEAD del contenido sin AAD = no hay binding path↔blob real (§ 5 vs § 15.3 G-01)

§ 15.3 detecta el gap G-01 y propone RF-25: `aad = "blob" | path_id`. Pero § 5 —la sección normativa de cripto— **nunca lo incorpora**: no define AAD para el cifrado de contenido. Si alguien implementa desde § 5, lo hace sin binding. La corrección tiene que estar **en la spec de cripto, no solo en el gap analysis**.

### 3. `revisions` con PK `(vault_id, rev)` impide más de un cambio por revisión (§ 3)

La PK `(vault_id, rev)` significa que cada revisión global del vault admite exactamente **un** `path_id`. Si un commit atómico toca N archivos (rename + edición, o snapshot restore de 500 archivos), necesitás N revisiones secuenciales. Eso genera huecos en el catch-up, obliga al cliente a hacer N roundtrips de fencing, y hace que un restore masivo cueste O(N) revisiones en vez de 1. Alternativa: PK `(vault_id, rev, path_id)` o batch de operaciones bajo una sola revisión.

### 4. El refcount de `blobs.refs` no tiene mecanismo de mantenimiento (§ 3, § 7)

No hay trigger, worker ni paso documentado que incremente/decremente `refs`. Si `refs` se actualiza por la aplicación, una interrupción entre crear la revisión y actualizar `refs` deja contadores inconsistentes → el GC borra blobs vivos o nunca libera los muertos. Necesitás: (a) documentar que `refs` se mantiene dentro de la misma transacción que toca `files`/`revisions`, o (b) reemplazarlo por un conteo derivado (`SELECT COUNT(*) FROM files WHERE blob_hash = ?` + `revisions` vivas).

---

## 🟡 Importante

### 5. scrypt N=32768 es débil para 2026 (§ 5)

N=32768 (2¹⁵) con r=8, p=1 da ~32 MB de memoria. OWASP recomienda **mínimo** N=2¹⁷ (131072) para passwords desde 2023. Los parámetros del oficial son de ~2020; copiarlos "por interop conceptual" no tiene sentido si no hay interop real. Un atacante con GPU moderna prueba ~10⁵ passwords/s con estos parámetros. Si se ofrece Argon2id como alternativa (mencionado de paso), definí parámetros concretos (m=64 MB, t=3, p=4 mínimo).

### 6. Sin versión de protocolo ni negociación en el handshake WS (§ 4.4)

El mensaje `hello` no lleva campo de versión. Cualquier cambio futuro al protocolo (nuevo campo, nuevo tipo de mensaje) no tiene forma de negociarse. Agregar `"proto": 1` en `hello` y `hello_ok` es trivial ahora y carísimo después.

### 7. La fase 4 (E2E) va después de la fase 3 (plugin desktop con sync funcionando) (§ 11)

Esto significa que toda la fase 3 se construye **sin cifrado**, y en la fase 4 hay que retrofittear cripto en el watcher, el state store, el SyncConnection y los frames binarios. El costo de retrofitting no está reconocido. Peor: si el modelo de datos del server (fase 1) asume paths en claro para debug/testing y después hay que blindarlo, los tests de las fases 1–3 dejan de servir. Lo más sano es meter al menos el stub de `CryptoProvider` (passthrough) desde la fase 1 y hacer E2E en la fase 3.

### 8. `keyhash_proof` en `hello` no está definido (§ 4.4)

El campo aparece en el ejemplo JSON del handshake pero no se define en ningún lado. ¿Es el `keyhash` directo? ¿Un HMAC sobre un challenge del server? Si es el keyhash directo, un replay del `hello` da acceso al vault. Si no hay challenge-response, al menos documentá que la autenticación depende exclusivamente del token de dispositivo y el keyhash es solo validación de "tengo la clave correcta".

### 9. Estimación de fase 7 (móvil) es fantasía (§ 11, § 12.4)

"2 sem (+dolor)" para iOS + Android, con cripto WASM, on-open/on-resume, reconciliación, memoria acotada, y testing en dos plataformas. § 12 reconoce "20% del código, 60% del dolor". Para un dev solo, 2 semanas no alcanza ni para el setup de CI de las dos plataformas. Realista: 6-8 semanas mínimo, o cortá scope a una sola plataforma.

### 10. Falta rotación de claves y migración de `enc_version` (§ 5)

Si la password se compromete, no hay procedimiento de rotación: hay que re-cifrar todo el vault con clave nueva, actualizar salt/keyhash en el server, e invalidar la recovery key vieja. Esto no está mencionado. RF-34 (§ 15.3) habla de migración de **versión** de cifrado, pero no de rotación por compromiso.

### 11. Recovery key: un blob en `_system/keybackup` es frágil (§ 5)

Si el blob se borra por GC, sync selectivo, o un bug de tombstone, el recovery muere. El backup de la master key debería estar en metadata del vault (tabla `vaults`, campo `recovery_blob`) fuera del árbol de archivos sincronizados, no ser "un archivo más".

---

## 🟢 Menor

### 12. BLAKE3 vs SHA-256 sin decidir (§ 1)

§ 1 dice "BLAKE3 (más rápido) o SHA-256". Dejarlo abierto obliga a negociar o asumir en cada componente. Si BLAKE3 es la elección, clavalo; si no, borrate la mención.

### 13. `CITEXT` no existe en SQLite (§ 3)

La nota dice "`CITEXT` → `TEXT` con collation `NOCASE`", pero `CITEXT` es una extensión de PostgreSQL que no todas las instalaciones tienen habilitada. Para el nivel B, usá `TEXT` con `UNIQUE` + `LOWER()` o `citext` como extensión explícita.

### 14. El diagrama de arquitectura (§ 2) tiene triple backtick roto

Hay un bloque ` ``` ``` ` vacío antes del diagrama ASCII, que cualquier renderer de Markdown va a interpretar mal.

### 15. El tipo de pago "Free" (§ 16.6 ítem 7) puede ser discutible

Si el plugin **requiere** un servidor propio para funcionar, el directorio podría clasificarlo como "Optional payment" (el usuario tiene costos de hosting). Verificá con la política del directorio si "self-hosted = free" aplica.

---

## Top 10 correcciones

1. **Mover el AAD `path_id` de § 15.3 (RF-25) a § 5** como parte normativa del cifrado de contenido — sin esto no hay binding real.
2. **Cambiar `path_enc` a nonce aleatorio** o eliminarlo — el cifrado determinista filtra igualdad y contradice "servidor ciego" (§ 5, § 2).
3. **Cambiar la PK de `revisions`** a `(vault_id, rev, path_id)` o agregar un campo `batch_id` para soportar operaciones atómicas multi-archivo (§ 3).
4. **Documentar el mantenimiento transaccional de `blobs.refs`** o reemplazarlo por conteo derivado para que el GC no corrompa datos (§ 3, § 7).
5. **Subir scrypt a N=2¹⁷ mínimo** o definir parámetros concretos de Argon2id — N=2¹⁵ es débil para 2026 (§ 5).
6. **Agregar `"proto": 1`** al handshake WS `hello`/`hello_ok` para negociación de versión (§ 4.4).
7. **Definir `keyhash_proof`**: qué es, cómo se genera, y si protege contra replay (§ 4.4).
8. **Mover la recovery key a metadata del vault** (fuera del árbol de archivos) para que no la mate el GC ni el sync selectivo (§ 5).
9. **Reestimar fase 7 (móvil) a 6-8 semanas** o acotar a una sola plataforma — 2 semanas es irreal para un dev solo (§ 11).
10. **Agregar sección de rotación de claves por compromiso** en § 5: procedimiento, re-cifrado, invalidación de recovery key vieja, y migración de dispositivos conectados.

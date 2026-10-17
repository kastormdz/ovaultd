# Mapa de implementación — contrato ↔ esquema ↔ bytes firmados

> Artefacto del **paso 1 de la fase 0.2** (`docs/build-plan.md`). Es la traducción entre los tres vocabularios del
> proyecto: los **nombres del protocolo** (`docs/protocol.md`), las **columnas reales** (`migrations/001_init.sql`) y los
> **bytes canónicos** que se firman (verificados por el spike en `spikes/crypto-interop`).
> Cuando algo no coincide, **esto es un bug de contrato**, no un detalle de implementación.

## 1. Nombres: protocolo ↔ base de datos

| Nombre en el protocolo | Dónde vive | Nota |
|---|---|---|
| `path_id` | `files.path_id` · `revisions.path_id` | HMAC con clave; **clave del índice** |
| `path_enc` | `files.path_enc` · `revisions.path_enc` | Se guarda en la historia: un dispositivo nuevo **no puede** derivar nombres desde HMACs |
| `blob_id` | `files.blob_hash` · `revisions.blob_hash` · `blobs.hash` | **Mismo valor, tres columnas.** `blob_id` = SHA-256 del **blob almacenado** (`nonce ‖ ciphertext ‖ tag`, ADR-0014) |
| `t_mac` | `files.t_mac` · `revisions.t_mac` | Firma del binding `path_id↔blob_id` |
| `sig` | `revisions.sig` | Firma del registro (autentica también `del`) |
| `epoch` | `vaults.epoch` · `revisions.epoch` | Rotación de `vault_key` (no de password) |
| `batch_id` | `batches.batch_id` (**no** en `revisions`) | Una fila en `batches` por revisión; `revisions` no la repite |
| `operation_id` | `revisions.operation_id` (índice único por vault) | Idempotencia real |
| `expected_rev` | `revisions.expected_rev` | **Se persiste** (ver § 4): es parte de los bytes firmados |
| `min_rev` / `next_since` | `vaults.min_rev` / calculado | Piso de retención y cursor de paginación |
| `prev_path_id` | `revisions.prev_path_id` | Metadata histórica de renames |

**No existen en la base (a propósito):** `content_hash` (es **local**, detección de cambios sobre plaintext),
`region` (eliminado, single-region), `plan`/tiers de cuota (opt-in), y ningún campo en claro del nombre o del contenido.

`op ∈ {add=1, mod=2, del=3, move=4}` — los mismos códigos en el protocolo y en los bytes firmados.

## 2. Creación del vault (resuelve G-1)

```
cliente                                            servidor
  │ generá vault_id = UUIDv7 (16 B)  ← ANTES de tocar la red
  │ kek = Argon2id(password, salt)
  │ wrapped_key_pw = AEAD(kek,           aad_pw, vault_key)
  │ wrapped_key_rc = AEAD(kek_recovery,  aad_rc, vault_key)
  │ pub/priv = Ed25519(k_sign)
  ├── POST /vaults {name, enc_version, salt, kdf_alg, kdf_params,
  │                 wrapped_pw, wrapped_rc, vault_pubkey, vault_id} ──►  crea la fila
  │                                                                     409 si el id ya existe
  └◄── {id}  (el id es EL MISMO que mandó el cliente)
```

**Regla dura:** el `vault_id` que entra en los AAD es **el del cliente**. El PK numérico interno del servidor **nunca**
se usa para derivar bytes. Si el servidor cambiara el id, **ningún** vector de la fase 0.2 volvería a verificar.

## 3. Push (CAS + idempotencia + cadena)

```
1. el cliente elige base = expected_rev   (la revisión sobre la que se apoyó)
2. firma sig = Ed25519(k_sign, msg_rec)   ← msg_rec INCLUYE expected_rev (ADR-0006)
3. push {operation_id, expected_rev, op, path_id, blob_id, t_mac, size, mtime, sig}
4. el servidor, en UNA transacción:
     a. valida autorización del device sobre el vault
     b. si operation_id ya existe  -> devuelve el ACK persistido, NO crea revisión
        (y si el mismo id llega con otro contenido -> rechaza)
     c. CAS: expected_rev == vaults.head_rev, si no -> t:conflict {base_rev, server_rev, client_rev}
     d. rev = head_rev + 1 ; inserta batches + N filas de revisions ; actualiza files ; blobs.refs += 1
     e. head_rev = rev ; persiste la respuesta idempotente
     f. commit  ← recién ahora
5. recién post-commit: ACK y notificación al resto
```

**Consecuencia fuerte del CAS contra `head_rev` (verificada al escribir este mapa):** los `push` quedan **serializados
por vault**. Un cliente que está atrás **no puede** pushear: tiene que hacer `pull` primero y rebasar. Es la
contrapartida exacta de que `expected_rev[i] == rev[i-1]` — la cadena solo se sostiene si no hay huecos. El algoritmo
del cliente ya lo respeta (`hello` → aplicar → push), pero es un límite real que hay que documentar en la UI
("hay cambios nuevos, primero sincronizo") y **no** un bug.

## 4. `expected_rev` se persiste (hallazgo al escribir este mapa)

`sig` cubre `expected_rev`, así que **sin guardarla la firma no se puede re-verificar después del push**: el servidor la
verifica en caliente y la pierde. Eso rompe la re-verificación de la historia y el anti-rollback después de un reinicio.
**Acción:** columna `revisions.expected_rev INTEGER NOT NULL DEFAULT 0` (0 = primer registro, sin base). Es 8 bytes por
revisión y compra verificación retroactiva.

## 5. Retención y paginación

| Concepto | Semántica |
|---|---|
| `since` | Última revisión **completa** aplicada por el cliente |
| `next_since` | Cursor que devuelve `changes`; **siempre** al final de una revisión completa |
| `min_rev` | Piso de retención (`vaults.min_rev`). Si `since < min_rev` ⇒ **resync completo**, nunca una delta silenciosa |
| Borrado | Tombstone con retención (default 30 días). Un tombstone que deja el archivo **a la vista** no borra nada: el cliente lo manda al trash |

## 6. Bytes canónicos (contrato verificado por el spike)

Estos son los **únicos** strings que se hashean/firman. Cambiar uno es un ADR nuevo.

| Uso | Bytes |
|---|---|
| Info HKDF de recovery | `"sync2/kek-recovery"` |
| Info HKDF de subclaves | `"sync2/" ++ label`, label ∈ `{content, nonce, path, meta, bind, sign}` |
| AAD de `wrapped_key_pw` | `"sync2/vaultkey/pw" ‖ vault_id(16) ‖ salt(16) ‖ u32be(m) ‖ u32be(t) ‖ u32be(p) ‖ u32be(dkLen)` |
| AAD de `wrapped_key_rc` | `"sync2/vaultkey/rc" ‖ vault_id(16)` |
| `nonce_body` | `HMAC-SHA256(k_nonce, plaintext)[0..24]` — viaja como **prefijo del blob** almacenado (ADR-0014) |
| AAD de `path_enc` | `path_id` (32 B) |
| AAD del contenido | vacío |
| `msg_bind` → `t_mac` | `"sync2/bind" ‖ vault_id(16) ‖ path_id(32) ‖ blob_id(32) ‖ u16be(epoch)` |
| `msg_rec` → `sig` | `"sync2/rec" ‖ u8(op) ‖ path_id(32) ‖ blob_id(32) ‖ t_mac(64) ‖ u64be(size) ‖ u64be(mtime) ‖ u64be(expected_rev) ‖ operation_id(16) ‖ u16be(epoch)` |

Enteros **big-endian**, sin separadores ni padding. `operation_id` = 16 B (UUID binario, no su texto).
`path` siempre en **NFKC** antes de hashear o cifrar.

## 7. Pendiente antes de la fase 1

| # | Qué | Por qué |
|---|---|---|
| 1 | Decidir **H-3** (AEAD incremental): aceptar O(n) con tope, o pasar a AEAD por chunk (ADR nuevo) | Bloquea el tamaño máximo de archivo |
| 2 | Definir el formato de la **respuesta idempotente** persistida (`operation_id` → ACK) | Es una tabla que no existe todavía |
| 3 | Decidir si el **checkpoint** del cliente guarda la `sig` del último registro | Es lo que hace útil el anti-rollback tras reinicio |
| 4 | Verificar las capacidades reales del adaptador de Obsidian (**G-5**: streaming, symlinks) | Fase 3, pero conviene medirlo antes de prometerlo |

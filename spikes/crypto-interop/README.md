# Spike de interop cripto (fase 0.2) — ✅ PASADO para `sync2`

Prueba que **Rust y JavaScript derivan las mismas claves, envuelven igual, cifran y firman byte a byte idéntico**
para la spec **`sync2`** (`docs/crypto.md`). Es la garantía que hace viable el server en Rust y el plugin en JS.
Los vectores están **congelados** en `tests/fixtures/crypto/sync2.json`.

```bash
./run.sh                        # corre ambos y diffea (no toca fixtures)
./run.sh --check                # ademas valida contra el fixture congelado
./run.sh --emit-candidates DIR  # candidatos para revisar ANTES de congelar
./run.sh --freeze               # regenera el fixture (solo tras revisar)
```

## Qué cubre

| Grupo | Verifica |
|---|---|
| NFKC | password en forma **descompuesta** (`e` + U+0301) → `é` en ambos lados |
| Argon2id | `m=65536 KiB, t=3, p=4, dkLen=32` → **KEK** (no clave de datos) |
| Envoltura | `wrapped_pw`/`wrapped_rc` con `aad` concatenado (ADR-0006) + **desenvolver** `vault_key` |
| HKDF | subclaves `sync2/{content,nonce,path,meta,bind,sign}` |
| Ed25519 | par determinista desde `k_sign`; la **pública** es idéntica en ambos lados |
| path_id / path_enc | HMAC con clave + AEAD con `nonce` aleatorio y **`aad = path_id`** |
| nonce_body | `HMAC-SHA256(k_nonce, plaintext)[0..24]` → **MLE keyed**: dedupe dentro del vault sin oráculo externo |
| blob_id | `SHA-256` del **ciphertext** (clave del CAS) |
| t_mac / sig | firma del binding `path_id↔blob_id` y del registro encadenado por `expected_rev` (ADR-0006) |
| Negativos | password incorrecta, AAD alterado, `path_enc` intercambiado, firma alterada, ciphertext truncado → `REJECTED` |
| Propiedades | mismo contenido → mismo `blob_id`; contenido distinto → distinto; **rename no cambia el `blob_id`** |

## Notas

- Lado cliente: **JS puro** (`@noble/hashes`, `@noble/ciphers`, `@noble/curves`), sin WASM ni addons nativos.
- ⚠️ `CARGO_TARGET_DIR` apunta **fuera** del repo. Nunca dejar `target/` acá adentro.
- `main.v1.bak.rs` y `spike.v1.bak.mjs` son el spike viejo de `sync1`: referencia histórica, **no** evidencia válida.

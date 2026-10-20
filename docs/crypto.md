# Spec cripto `sync2` (vigente)

> Estado: vigente desde 2026-09-25. Sustituye a `sync1` (ver **ADR-0002**). Cambiar esto requiere un ADR nuevo.
> Estado de la interop: probada para `sync2` (2026-09-25). Rust y JS derivan, envuelven, cifran y firman byte a
> byte idéntico, y los vectores están congelados en `tests/fixtures/crypto/sync2.json`. Los vectores `sync1` (más abajo)
> quedan como referencia histórica y por el pitfall de NFKC. El layout canónico de los bytes firmados es **ADR-0006**.

## Dominio y versionado

- `DOMAIN = "sync2"`. Prefija los `info` de HKDF → cambiar de dominio cambia todas las claves (válvula de escape).
- `enc_version = 2` para vaults nuevos. `enc_version = 1` = `sync1` (histórico). `0` = cifrado gestionado por el server (no usado).

## Arquitectura de claves: password, KEK y clave de datos

```
password_nfkc = NFKC(password)
kek           = Argon2id(pw_utf8, salt, m=64 MiB, t=3, p=4, dkLen=32)     # KEK, NO clave de datos
vault_key     = 256 bits ALEATORIOS (se generan al crear el vault)
wrapped_pw    = XChaCha20-Poly1305(kek,        aad="vaultkey/pw" § vault_id § kdf_params, msg=vault_key)
wrapped_rc    = XChaCha20-Poly1305(kek_recovery, aad="vaultkey/rc" § vault_id,                msg=vault_key)
kX            = HKDF-SHA256(ikm=vault_key, salt=salt, info="sync2/"+X, L=32)
```

**Regla estructural:** la clave de datos es aleatoria y viaja envuelta. La password sólo protege su envoltura.
Consecuencia: **rotar la password no cambia `path_id`, `blob_id` ni los MACs**: se re-envuelve un registro (§ Rotación).

| Subclave | Uso |
|---|---|
| `k_content` | cifrado del contenido de los blobs |
| `k_nonce` | derivación del nonce determinista *keyed* (separada de `k_content`) |
| `k_path` | `path_id` (HMAC) + `path_enc` (AEAD) |
| `k_meta` | metadatos sensibles que el server no debe leer (a futuro) |
| `k_bind` | (reservada) material de binding auxiliar |
| `k_sign` | seed → par Ed25519 determinista del vault |

**Qué viaja al server:** `salt`, `kdf_alg`/`kdf_params`, `wrapped_pw`, `wrapped_rc` y la **clave pública** Ed25519.
**Nunca** la password, `kek`, `vault_key` ni subclaves. Un `keyhash` opcional sirve como verificación previa, no como proof.

## Estructuras

```
verifier   = SHA-256(HKDF(vault_key,"sync2/keyhash"))            # 32 B (opcional, verificación previa)
path_id    = HMAC-SHA256(k_path, NFKC(path)_utf8)  [0..32]        # índice server-side, determinista
path_enc   = XChaCha20-Poly1305(k_path, nonce=24 B RANDOM, aad=path_id, msg=NFKC(path))
nonce_body  = HMAC-SHA256(k_nonce, plaintext)       [0..24]        # MLE keyed → dedupe dentro del vault
blob        = nonce_body ‖ XChaCha20-Poly1305(k_content, nonce=nonce_body, aad="", msg=contenido)
                                                                   # ADR-0014: el nonce viaja DENTRO del blob
blob_id     = SHA-256(blob almacenado)                             # clave del CAS (sobre el blob con su sobre)
T          = Ed25519_sign(priv, vault_id § path_id § blob_id § epoch)   # binding path↔contenido
sig        = Ed25519_sign(priv, rev § op § path_id § blob_id § T § epoch)  # firma del registro del oplog
```

## Decisiones y por qué

1. Vault key aleatoria envuelta (KEK). Con la clave de datos derivada directo de la password, rotar la password
   cambiaba `path_id` (HMAC de `k_path`) → había que migrar el **índice**, no sólo los cuerpos. Envolver una clave
   aleatoria deja la rotación en "re-envolver un registro".
2. `path_id` = HMAC con clave, determinista a propósito. El server puede indexar y deduplicar sin adivinar la
   estructura del vault (un SHA-256 pelado cae por diccionario: los paths son predecibles).
   Fuga aceptada: el server observa **igualdad y estabilidad** de paths, nunca sus nombres.
3. `path_enc` con nonce ALEATORIO y `aad = path_id`. Un cifrado determinista filtraba igualdad; y sin atar
   `path_enc` a `path_id` el server podía cambiar el nombre cifrado y presentar el contenido correcto bajo otro
   nombre sin invalidar `T` (era un agujero del binding).
4. Binding en metadata firmada (`T`), no en el AAD del cuerpo. Con `aad = path_id` en el **contenido**, el dedupe
   por contenido moría y cada **rename** obligaba a re-cifrar y resubir el archivo. Con `T` firmado: rename = recalcular
   32 B de metadata, dedupe intacto, y el server no puede mover un blob a otro path.
5. `blob_id` sobre el ciphertext + nonce MLE *keyed*. El CAS deduplica dentro del vault, y sin `k_nonce`
   nadie puede confirmar contenido candidato desde fuera. El nonce determinista exige clave separada (`k_nonce`), no
   reutilizar `k_content`.
6. Ed25519 derivado del vault + registros firmados. Con un `keyhash` compartido, el server guardaba material
   equivalente para forjar proofs (y una afirmación ingenua de "nunca viaja crudo" era falsa). Con firma: el server
   guarda sólo la pública; y firmar cada registro autentica también los borrados (antes silenciables) y habilita
   anti-rollback: el cliente rechaza cualquier historia que retroceda respecto de su último `rev` firmado.
7. Argon2id en vez de scrypt `N=32768`. Los parámetros del servicio oficial son de ~2020 y no hay interop que los
   justifique: `N=2^15` ≈ 32 MB es débil para 2026. Parámetros versionados por vault (`kdf_alg`, `kdf_params`) para
   endurecerlos sin romper vaults viejos.
8. JS puro en el cliente (`@noble/hashes` + `@noble/ciphers` + `@noble/curves`): una sola implementación
   desktop/móvil, auditable, sin binarios opacos. **No** porque WASM esté prohibido (no lo está).
9. Chunks: MVP en dos pasadas. El nonce depende del plaintext completo → 1.ª pasada hash, 2.ª cifrado por chunks
   (memoria acotada, sin prometer delta por línea). AEAD por chunk con manifiesto: post-MVP.
10. Recovery con entropía explícita. `vault_key` + un secreto **aleatorio de 256 bits** codificado en base32 con
    checksum. La codificación **no** es seguridad: la entropía la da el CSPRNG.

## Fugas aceptadas (explícitas, no accidentales)

| Fuga | Alcance |
|---|---|
| Igualdad/estabilidad de **paths** (`path_id` determinista) | El server ve *cuándo* dos revisiones tocan el mismo path; nunca el nombre |
| Igualdad de **contenido** (dedupe por `blob_id`) | Dentro del mismo vault |
| **Dedupe oracle** entre miembros de un vault compartido | Inherente a compartir claves; fuera del vault no aplica |
| **Tamaños, mtimes, cantidad de archivos, dispositivo** | Metadata inevitable del modelo |

> **Alternativa legítima (no elegida):** nonce aleatorio por blob + renunciar al dedupe. Gana confidencialidad, pierde
> deduplicación y reanudación por hash. Es el punto de cambio único en § 5.1 de la spec.

## Rotación

1. KEK nuevo (salt y `kdf_params` nuevos) desde la password nueva.
2. Re-envolver `vault_key` (un registro). `path_id`, `blob_id` y `T` **no cambian**: el índice sobrevive.
3. Re-envolver/regenerar `wrapped_rc`.
4. Revocar tokens de dispositivo y exigir re-auth.
5. Sólo si se sospecha `vault_key` comprometida: subir `epoch` y re-cifrar cuerpos + recalcular MACs (operación
   cara y explícita).

**Limitación:** invalidar una recovery no revoca copias que alguien haya guardado de un `wrapped_rc` viejo.

## Recovery key

`vault_key` + secreto aleatorio de 256 b (base32 + checksum) → `wrapped_rc`, guardado en **metadata del vault**
(`vaults.wrapped_key_rc`), **no** como archivo en el árbol: el GC y el sync selectivo no lo pueden matar.
Sin esto, perder la password = perder el vault (el pecado del servicio oficial).

## Vectores de prueba

### `sync1` (históricos, no aplican a `sync2`)

Se conservan porque documentan el método y el pitfall de NFKC:

```
password  = "contrase\u00f1a-\u0065\u0301-\u6771\u4eac"     # forma DESCOMPUESTA (e + U+0301)
NFKC      = "contraseña-é-東京"
MASTER      635766a8c67aaee168c70e06b1093309f7a8bb06639f645994dc5915086c0a8d
PATH_ID     d97d3566d75ab1979545a8516b3865d4d11d5e4db76565ba0c4a68dcbba33b5d
ROUNDTRIP   OK
```

### `sync2` (congelados el 2026-09-25)

Generados por `spikes/crypto-interop`: **Rust y JS coinciden byte a byte**, y el fixture está congelado en
`tests/fixtures/crypto/sync2.json`. Se regeneran **solo con revisión** (`./run.sh --emit-candidates DIR` → revisar →
`./run.sh --freeze`), y `./run.sh --check` falla ante cualquier diferencia. El layout canónico de los bytes firmados
está en **ADR-0006**.

> Entrada del vector: `salt=spike crypto-interop (rust)`, `vault_id=0102…0f10` (fijo de test), `vault_key=00..1f` (fija de test),
> Argon2id `m=65536 KiB, t=3, p=4`, plaintext de 39 B, `expected_rev=1834`, `epoch=1`.

**Derivación y envoltura**

| Vector | Valor |
|---|---|
| `PW_NFKC` | `contraseña-é-東京` |
| `KEK` | `4183554e9806554f24fe6552de3a67f12f94c81b19ee51ca5db7d9ceaea923c0` |
| `VAULT_KEY` | `000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f` |
| `WRAPPED_PW` | `ce786c560ec13a31111525c17b34c8d1e94730244115249d76f74e60ae5480a87f8d6e7988a733bc0c097b46648af7c7` |
| `WRAPPED_RC` | `79345749393b62ae435c99989ce80ed7bbaa45ce0b53f712fe7308d9dc2b0087d95331e31b6b84b85d3b45d69544a5c5` |
| `UNWRAP` | `OK` |
| `K_CONTENT` | `3120498841f07cd1c89d1a78431b62a9f3c0958ca86d0d3aace113a6bb7f23f7` |
| `K_NONCE` | `874eba2d58f9234343e9c607f677d2e2b3d0bca3cfadbcd453007eb0b59215b0` |
| `K_PATH` | `0e904775a7fe090e8321eaa4b442a3f58a69f2352194019042b777c26e5d1d07` |
| `K_META` | `e39b902a0f19c52f95514322c2461b2df0bb93a6990a81b0ecd4e2c519644801` |
| `K_BIND` | `708a9a7e57786019a78e05feb2fccc85056a553d69f2522bcbe74263a9870ba7` |
| `K_SIGN` | `172baccf9cd69261fc3348b7a61d6d5dc7bab29b3d660df72ed606a6cefc8817` |

**Identidad y paths**

| Vector | Valor |
|---|---|
| `PUBKEY` | `77d8460a2ea085b9f4b7a3c60906034cbeadd56e54fb0fa82daae33abf42832d` |
| `PATH_ID` | `f3cb55a66d6ddf459a0b2400b281cfc32cb7db434ccd698276a986a534f0cb29` |
| `PATH_ENC` | `ab8810b8b99d4e5fe8c205164e7008f24cc5f969347623953fba22b29276942d480980ced81546566f9e552117a02cb591ea5186d141bf645353cef859a57132bf3d8f8df5217d` |
| `PATH_ENC_RT` | `OK` |

**Contenido y CAS**

| Vector | Valor |
|---|---|
| `NONCE_BODY` | `227434e610f319897fe39488704b4a3e3c57c41e83de38e2` |
| `BLOB` | `227434e610f319897fe39488704b4a3e3c57c41e83de38e2afb72fa5d646f6e93de0f26f229790bc9243fd7612b8c74385c79fa19678a88fa820ff15319db50f63bc616e03368d705ec308d504703b` |
| `BLOB_LEN` | `79` |
| `BLOB_ID` | `e7ce4c1f23dd7b5a47149c2c6fd06fd0d635a23c32c5f3663154eddd2eae8fc0` |
| `BLOB_RT` | `OK` |

**Binding y firma**

| Vector | Valor |
|---|---|
| `T_MAC` | `f576e7ac4ba5644c0c6625ce8d628ab137dc528911fcb7301ba3269c26a48332900debc4be32dd838bb07eab9e26478d2982f0215d635a00a9f926ea04586d06` |
| `SIG` | `d50a2dcafaa25cd7d498e09f1c545fcec48dcac922825dab71195a50da71d2fc007d1173fe29089b40ba166b9eaaf98f6782f4ba5d9d653f366eef71e0fc4803` |
| `SIG_VERIFY` | `OK` |

**Negativos y propiedades**

| Vector | Valor |
|---|---|
| `NEG_WRONG_PW` | `REJECTED` |
| `NEG_AAD` | `REJECTED` |
| `NEG_SWAP_PATH` | `REJECTED` |
| `NEG_SIG` | `REJECTED` |
| `NEG_TRUNC` | `REJECTED` |
| `DEDUPE_SAME` | `SAME` |
| `DEDUPE_DIFF` | `DIFFERENT` |
| `RENAME` | `OK` |

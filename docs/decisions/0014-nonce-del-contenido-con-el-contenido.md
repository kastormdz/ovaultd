# ADR 0014 — El nonce del contenido viaja con el contenido (y re-congelado de vectores)

- **Fecha:** 2026-10-16
- **Estado:** aceptado
- **Origen:** al portar la cripto para el plugin (fase 3) apareció que **ningún lector puede
  descifrar un contenido que no tenga ya en claro**. Nadie lo había visto porque el gate de
  interop solo verifica el lado de **cifrado** (self-consistente); no existía ningún camino de
  lectura en ninguna implementación.

## Contexto

`sync2` cifra el contenido así (ADR-0009 § 2, `docs/crypto.md`):

```
nonce_body = HMAC-SHA256(k_nonce, plaintext)[0..24]     # MLE keyed: dedupe dentro del vault
blob       = XChaCha20-Poly1305(k_content, nonce=nonce_body, aad="", msg=contenido)   # BARE
blob_id    = SHA-256(blob)
```

ADR-0009 decidió que el contenido se guarda **bare** (sin el nonce) porque "el nonce es
derivable". Es derivable **para el escritor** — que tiene el plaintext. Para el **lector** es
circular: para descifrar necesita el nonce, para derivar el nonce necesita el plaintext, y el
plaintext es exactamente lo que no tiene. No hay atajo: el tag de XChaCha20 exige el nonce
correcto, y ninguna función de las que el lector conoce (`blob_id`, `t_mac`, path, revs) lo
determina.

Consecuencia real (bloqueante para la fase 3): un device B que baja una nota creada por el
device A **no puede abrirla** — ni el propio A desde una instalación nueva. El único que puede
descifrar un blob es quien lo cifró y conservó el plaintext.

## Decisión

El blob de contenido se guarda con el **mismo sobre que la envoltura y el `path_enc`**:

| Dato | Formato almacenado | Cambia |
|---|---|---|
| `blob` (contenido) | `nonce[24] ‖ ciphertext ‖ tag[16]`, `nonce = nonce_body(k_nonce, plaintext)` | **sí** (antes bare) |
| `blob_id` | `SHA-256(blob almacenado)` | valor nuevo (misma semántica) |
| `size` (revisiones/firmas) | largo del **plaintext** | no |
| sobre | `nonce + tag = 40 B` | — |

- El nonce sigue siendo **determinista** (MLE): mismo plaintext ⇒ mismo nonce ⇒ mismo blob ⇒
  mismo `blob_id` ⇒ **el dedupe no cambia**.
- El lector: lee 24 B de prefijo, `open(k_content, nonce, resto)`. El nonce es público: es una
  imagen de PRF bajo `k_nonce` (secreto y por vault), y la igualdad de plaintexts **ya** era
  observable por `blob_id` — no agrega fuga.
- El **wire y el server no cambian**: los blobs son opacos para el servidor.
- Los límites de subida se ajustan: el tope de CONTENIDO sigue siendo `MAX_FILE_BYTES` (32 MiB
  de plaintext), así que el tope del **cuerpo cifrado** es `MAX_FILE_BYTES + 40`.

## Re-congelado (proceso, no excepción)

Los vectores dorados (`tests/fixtures/crypto/sync2.json`) se **regeneran** con
`spikes/crypto-interop/run.sh --freeze` — el camino sancionado de la fase 0.2 — después de
re-verificar la interop Rust↔JS con el formato nuevo (`./run.sh`, diff limpio). La regla "la CI
nunca los regenera" sigue viva: la regeneración es un acto explícito, documentado y gateado
(freeze.py se niega a escribir si los negativos no dan `REJECTED` o un roundtrip no da `OK`).
Los valores de `BLOB`, `BLOB_LEN`, `BLOB_ID`, `T_MAC`, `SIG` y `NONCE_BODY` cambian; las claves,
el KDF, las envolturas y los paths **no**.

## Consecuencias

- `encrypt_blob` devuelve el sobre completo; `decrypt_blob` toma el blob almacenado (el nonce
  sale del prefijo). `decrypt_blob_with_nonce` queda para vectores (bare + nonce explícito).
- ADR-0009 § 2 queda **superado en su fila de contenido** (se agrega addendum).
- El pull+descifrado queda cubierto por tests (Rust y el port TS), incluido el caso que lo
  destapó: leer con **subclaves recién derivadas**, sin ningún estado del escritor.

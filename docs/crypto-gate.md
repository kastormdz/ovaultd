# Gateway cripto `sync2` — evidencia (fase 0.2)

- **Fecha:** 2026-09-25
- **Resultado:** ✅ **`INTEROP sync2 OK`** — Rust y JS derivan, envuelven, cifran y firman **byte a byte idéntico**.
- **Contrato de bytes:** `docs/decisions/0006-g1-g2-vaultid-y-firma-encadenada.md`
- **Vectores congelados:** `tests/fixtures/crypto/sync2.json` (32 vectores)

## Qué prueba (y qué no)

| Cubre | No cubre |
|---|---|
| Interop de **primitivas y derivación** entre los dos lenguajes | Auditoría criptográfica formal |
| Que la envoltura de `vault_key` **desenvuelve** con el KEK correcto y **falla** con otro | Resistencia a un atacante con acceso al disco del cliente |
| Que `path_enc` está atado a su `path_id` (AAD) y **no se puede intercambiar** | Que el plugin de Obsidian pueda usar estas APIs en su runtime (fase 3) |
| Que el nonce determinista **deduplica** dentro del vault y **renombra sin resubir** | Análisis de fugas por tiempos y tamaños |
| Que los registros firmados verifican con la **pública** del vault | Completitud frente a un servidor que **oculta** registros |

## Cómo se corre (y qué tiene que decir)

```bash
cd spikes/crypto-interop
./run.sh                        # corre ambos lados y diffea (no toca fixtures)
./run.sh --check                # + valida contra el fixture congelado  <- el gate real
./run.sh --emit-candidates DIR  # candidatos para revisar ANTES de congelar
./run.sh --freeze               # regenera el fixture (solo tras revisar los candidatos)
```

Salida esperada del gate:

```
==> INTEROP sync2 OK: Rust y JS derivan, cifran y firman byte a byte identico y coinciden con el fixture congelado
```

`freeze.py` **se niega a escribir** si los negativos no dan `REJECTED`, si un roundtrip no da `OK` o si falta un vector.
Un fixture que no pasa su propio gate es peor que no tener fixture.

## Resultados (32 vectores)

| Grupo | Veredicto |
|---|---|
| Derivación y envoltura (Argon2id → KEK, `wrapped_pw`/`wrapped_rc`, desenvuelta, 6 subclaves HKDF) | **idéntico** |
| Identidad y paths (pública Ed25519, `path_id` HMAC, `path_enc` con `aad = path_id`, roundtrip) | **idéntico** |
| Contenido y CAS (nonce determinista, `blob`, `blob_id`, roundtrip) | **idéntico** |
| Binding y firma (`t_mac`, `sig`, verificación con la pública) | **idéntico** |
| Negativos: password incorrecta · AAD alterado · `path_enc` intercambiado · firma alterada · ciphertext truncado | **`REJECTED` en los dos lados** |
| Propiedades: mismo contenido → mismo `blob_id` · contenido distinto → distinto · rename conserva `blob_id` | **`SAME` / `DIFFERENT` / `OK`** |

Detalle: Rust `argon2` 0.6 + `ed25519-dalek` 3.0 + `chacha20poly1305` 0.10 · JS `@noble/hashes` 2.4 + `@noble/ciphers` 2.4
+ `@noble/curves` 2.4 (JS puro, sin WASM).

## Hallazgos del gateway

### H-1 · El acuerdo entre dos implementaciones **no** prueba corrección (🔴 encontrado y cerrado)

El prompt de generación fijaba `size = 43` para el registro firmado. El plaintext real mide **39 bytes**.
Rust lo escribió como literal y JS como constante: **coincidieron**, la interop pasó… y el vector era **falso**.

El diff Rust↔JS **no podía** detectarlo, porque el error era idéntico en ambos lados. Se detectó **recalculando la
longitud con un script aparte** (`python3 -c "len(...)"`), que es lo único que lo caza.

**Acción tomada:** el gate ahora valida **valores esperados** (negativos, roundtrips, propiedades), no sólo igualdad
entre lados; y la consigna de trabajo incluye "recalcular toda constante derivada antes de congelar".

### H-2 · Slop del modelo barato (informativo)

En 765 líneas de cripto, `gpt-5.6-luna` produjo **4 defectos, todos mecánicos**: 1 error de compilación (ambigüedad de
tipo con `.into()`), 2 campos con formato incorrecto (imprimía el hex descifrado en lugar de `OK|FAIL`) y 1 constante
copiada del prompt. ~0,5%. Lo caro no es arreglarlos: es **detectarlos**.

### H-3 · G-6 (AEAD incremental) — medido, pendiente de decisión

XChaCha20-Poly1305 **no** expone una API incremental. Medido con archivos de **8 / 64 / 256 MiB** en proceso limpio
(`probe-o1.mjs`, node 22.23, chunks de 64 KiB):

| | Resultado |
|---|---|
| **Pasada 1** (nonce, HMAC incremental) | el pico **no escala** con el tamaño: **+8,1 MiB** (8 MiB) · **+14,1 MiB** (64 MiB) · **+7,2 MiB** (256 MiB) sobre la base de node (39,5–66 MiB) ⇒ **O(1)** ✅ |
| **Pasada 2** (AEAD de mensaje único) | el pico **sí escala**: **347 MiB** de RSS y **257 MiB** de `arrayBuffers` para 64 MiB ⇒ **~4× el archivo, O(n)** ❌ |
| Nonce con chunks de 1 KiB / 64 KiB / 1 MiB / buffer completo | **OK** — la partición no cambia el nonce (es lo que habilita la pasada 1 acotada) |
| Modificación del archivo entre pasadas | **ABORT** (tamaño + hash + mtime): nunca publica ciphertext de un archivo que cambió |
| `blob`/`blob_id` de dos pasadas vs mensaje completo | **idéntico** (`BLOB_MATCH OK`) |

**Corrección de una medición previa:** el primer run reportaba `PEAK_RSS_PASS1 = 81,45 MiB`, **contaminado** por el buffer
de 64 MiB que el propio script usaba para *generar* el archivo de prueba. Medido aparte —proceso limpio, tres tamaños—
la pasada 1 no escala. El script ya reporta `BASE_RSS_MIB` y **deltas**, así que la contaminación queda a la vista:
corrida corregida (64 MiB) → `BASE_RSS_MIB 80,93` · `PEAK_RSS_PASS1_DELTA_MIB 0,13` · `PEAK_RSS_PASS2_DELTA_MIB 225,54` ·
`PEAK_ARRAYBUFFERS_PASS2_MIB 257,01`.

Moraleja: un número que no se mide aislado es un número que miente.

**DECIDIDO (2026-09-25) → `docs/decisions/0007-contenido-soportado-y-tope-de-tamano.md`:** opción **(1)**, tope duro de
**32 MiB** de contenido. El alcance del MVP son **notas**, así que el O(n) queda acotado y predecible (~130 MiB de pico
por subida con un blob en vuelo) sin cambiar `sync2`. La opción (2) —AEAD por chunk— queda documentada como el camino a
seguir **sólo si** el caso de uso cambia; no se traslada el problema al plugin (fase 3).

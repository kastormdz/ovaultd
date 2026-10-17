# ADR-0009 — Formato almacenado de la clave envuelta y del `path_enc` (y el nonce que no se copia)

- **Fecha:** 2026-09-25
- **Estado:** **aceptado** (el primer punto lo exige el vector congelado; el resto corrige defectos que el gate destapó)
- **Origen:** escribir el gate real de `ovaultd-crypto` destapó tres defectos que un test **vacuo** estaba tapando

## 1. El AAD de `wrapped_key_pw` **incluye el salt** (defecto corregido)

El crate armaba el AAD así:

```
"sync2/vaultkey/pw" ‖ vault_id ‖ m ‖ t ‖ p ‖ dk_len          ← FALTABA el salt
```

y la spec (`docs/implementation-map.md` § 6, verificada por el spike) dice:

```
"sync2/vaultkey/pw" ‖ vault_id ‖ salt(16) ‖ u32be(m) ‖ u32be(t) ‖ u32be(p) ‖ u32be(dk_len)
```

**Consecuencia:** la clave envuelta no quedaba atada al `salt` que se usó para derivar la KEK, así que dos vaults
con el mismo `vault_id` y distinto salt compartían AAD. Lo cazó el vector `WRAPPED_PW`: no desenvolvía.
**Regla:** todo parámetro que entra en la derivación de la clave que envuelve **también** entra en el AAD, o el
binding es decorativo.

## 2. Formato almacenado: el nonce **va adentro** para clave y path, y **no** va para contenido

| Dato | Formato almacenado | Por qué |
|---|---|---|
| `wrapped_key_pw` / `wrapped_key_rc` | `nonce[24] ‖ ciphertext ‖ tag[16]` | El nonce es aleatorio: si no se guarda, la clave no se puede desenvolver |
| `path_enc` | `nonce[24] ‖ ciphertext ‖ tag[16]` | Idem: no hay de dónde derivarlo |
| `blob` (contenido) | `nonce[24] ‖ ciphertext ‖ tag[16]` (**ADR-0014**: antes bare) | El nonce era "derivable" solo para el ESCRITOR; el LECTOR no puede (circular). Se guarda adentro, igual que los otros dos. El dedupe no cambia: el nonce sigue siendo `HMAC-SHA256(k_nonce, plaintext)[0..24]` |

Esto **resuelve una ambigüedad real**: el esquema tiene `wrapped_key_pw BLOB` y `path_enc BLOB` sin columna de nonce,
así que el nonce solo puede ir adentro o ser determinista. Va adentro.

**Consecuencia sobre los vectores dorados:** los `WRAPPED_PW`, `WRAPPED_RC` y `PATH_ENC` del fixture son el
**ciphertext pelado** con el nonce fijado por el test (artefacto del spike). El gate los verifica
**descifrándolos** con ese nonce explícito, no reproduciéndolos. Si alguna vez se re-congelan, conviene emitirlos en el
formato almacenado completo.

## 3. ⚠️ El nonce fijo del test **no** es una constante de producción

El spike usaba `nonce = [0x01;24]` para la envoltura de password, `[0x02;24]` para la de recovery y `[0x03;24]` para
**todos** los `path_enc`. Tomado como diseño, eso sería **reutilización de (clave, nonce)** en ChaCha20-Poly1305: mismo
par para todos los paths del vault ⇒ mismo keystream ⇒ el XOR de dos cifrados revela el XOR de los paths, y el
Poly1305 con clave reusada habilita falsificación.

**Regla:** en producción la envoltura y el `path_enc` usan **nonce aleatorio** (guardado en el blob, punto 2); el
contenido usa el nonce **derivado** del plaintext. Los valores fijos existen **solo** dentro del vector de test.

## 4. Pendiente menor: `msg_rec` recibe el `op`, no un "`enc_version`"

`crates/ovaultd-crypto` nombra `enc_version: u8` al byte que la spec define como **`u8(op)`**
(`Add=1, Mod=2, Del=3, Move=4`). El valor es el mismo (`0x02`), así que **los vectores no pueden detectarlo**: es un
error de nombre que se vuelve un error de semántica en cuanto alguien pase ahí la versión de encriptado de verdad.
Renombrar a `op` (o aceptar `Op`) antes de la fase 2.

## 5. Lo que este ADR NO cambia

- `sync2` **no** cambia: no se tocó ninguna primitiva, ninguna etiqueta HKDF ni ningún layout de bytes firmados.
- Los 32 vectores siguen siendo los mismos y el fixture **no** se regeneró (mismo hash).
- El esquema **no** necesita migración: el nonce dentro del blob no agrega columnas.

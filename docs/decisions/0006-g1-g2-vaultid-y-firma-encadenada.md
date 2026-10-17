# ADR 0006 — Resolución de G-1 y G-2: `vault_id` del cliente y firma encadenada

- **Fecha:** 2026-09-25
- **Estado:** aceptado · **cierra** los huecos G-1 y G-2 de § 17.7
- **Origen:** hallazgos del plan de construcción (`docs/build-plan.md`, paso 2 de la fase 0.2), que exigió
  cerrarlos **antes** de generar vectores dorados

## Contexto

El plan detectó dos preguntas que la spec no respondía y que **bloquean la generación de vectores** (y por lo
tanto toda la fase 1):

- **G-1:** `wrapped_pw = AEAD(kek, aad = "…" ‖ vault_id, …)` se calcula en el cliente, pero el `vault_id` lo
  asignaba el servidor. Huevo y gallina: no se puede envolver antes de crear.
- **G-2:** `sig` cubría `rev`, y la `rev` la asigna el servidor en el commit. El cliente no puede firmar un
  valor que todavía no existe.

## Decisión

### G-1 — el `vault_id` lo genera el cliente (UUIDv7)

1. El cliente genera **`vault_id` = UUIDv7 (16 B)** **antes de cualquier llamada de red** y lo manda en la
   creación del vault.
2. El servidor lo usa como **identificador público** y puede mantener un PK numérico interno, pero **nunca**
   entra en el AAD: el AAD se calcula con el id del cliente.
3. Colisión: el servidor responde `409` y el cliente reintenta con otro UUID.

**Por qué:** el AAD tiene que ser computable **offline** (el plugin crea el vault local y sincroniza después; si
el id viniera del servidor, crear un vault exigiría red). Y el `vault_id` **no es una frontera de seguridad** —
lo que autentica es la clave pública y las firmas, no la procedencia del id.

### G-2 — firma encadenada: se firma la base (`expected_rev`), no la `rev` asignada

`sig = Ed25519(k_sign, H(canonical(op, path_id, blob_id, t_mac, size, mtime, expected_rev, operation_id, epoch)))`

- **No hay round-trip extra** (`prepare`/`commit`): el cliente firma lo que ya sabe antes de enviar.
- Como cada registro firma la **revisión base** sobre la que se apoyó, los registros forman una **cadena**:
  `expected_rev` de un registro debe ser la `rev` asignada al anterior. Un servidor malicioso no puede
  **reordenar ni reproducir** registros sin romper la cadena.
- El **anti-rollback** se apoya en el checkpoint del cliente (última `rev` + `sig` vista): una historia que
  retroceda se detecta.

## Consecuencias

- **Residual documentado (no prometer de más):** la `rev` asignada **no está firmada**. Un servidor malicioso
  puede **re-etiquetar** una `rev` dentro de los límites que la cadena permite (debe seguir satisfaciendo
  `expected_rev[i] == rev[i-1]`), y no se prueba **completitud** (puede ocultar registros). Es el techo real
  del modelo, y es más honesto decirlo que agregar más firmas que no lo cierran.
- Los **vectores dorados** usan un `vault_id` fijo de test y una `vault_key` fija de test (nunca aleatorios en un
  vector): reproducibilidad del vector ≠ seguridad del runtime.
- Este ADR **debe** quedar reflejado en `docs/crypto.md` (layout canónico de los bytes firmados) antes de la
  fase 1: los layouts del spike son la fuente de verdad de los vectores.

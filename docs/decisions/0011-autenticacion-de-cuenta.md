# ADR-0011 — Autenticación de cuenta: password propia, **independiente** de la password E2E

**Estado:** aceptado (2026-09-26).
**Contexto:** `docs/protocol.md` promete `POST /api/v1/auth/signin {email, password, mfa?}`, pero el esquema no tenía
columna de password, `auth.rs` no verificaba ninguna y el `argon2` de `Cargo.toml` no lo usaba nadie. Quedaba sin
resolver qué autentica la **cuenta** (quién crea y ve vaults) frente a qué autentica el **vault** (quién prueba la
posesión de la `vault_key`).

## Decisión

1. **La cuenta se autentica con password** (`Authorization` de control, `POST /auth/signin`) y el servidor la persiste
   **hasheada con argon2id** en `accounts.password_hash` (string PHC: parámetros y salt adentro). El token que devuelve
   es el de un **device** (`device_tokens`, con vencimiento y revocación individual).
2. **La password E2E es OTRO concepto**: es el secreto que alimenta `Argon2id(password, salt)` para derivar la KEK que
   envuelve la `vault_key`, y **nunca se manda al servidor en ninguna forma**. Lo exige `build-plan.md` §Fase 4:
   *"password de cuenta y password E2E son conceptos independientes"*.
3. **Lo que esto acepta explícitamente** (y por qué es aceptable): un servidor comprometido que capture la password de
   cuenta **en un login** puede derivar la KEK **sólo si el usuario reusó la misma password** para las dos cosas. Con las
   dos independientes, la password de cuenta no abre ningún contenido. **Por eso la independencia no es una sugerencia:
   es la propiedad que sostiene la E2EE.** El onboarding (Fase 3) tiene que pedir las dos por separado, explicarlo, y
   **no** ofrecer "usar la misma" como default. Un default cómodo acá borra la garantía entera.
4. **El servidor no ve jamás** la password E2E, la `vault_key`, ni material del que se pueda derivar contenido. Esta ADR
   no cambia el modelo de servidor ciego: sólo agrega un factor de autenticación de cuenta, ortogonal al cifrado.
5. **MFA queda declarado, no simulado.** `mfa` viaja en el cuerpo de `signin` y, mientras no esté implementado, su
   presencia devuelve `not_implemented`: **jamás se ignora en silencio**, porque un cliente que manda un código y recibe
   un login exitoso cree estar protegido por un segundo factor que nadie miró.

## Alternativas consideradas

- **Firma pura (sin password de cuenta)**: el login sería challenge + firma con la clave del vault y el servidor no vería
  ningún secreto. Más fuerte, pero **el documento promete email+password** y el enrolamiento de un device nuevo queda sin
  camino claro (un device sin clave no puede firmar). Descartada por decisión de producto, no por debilidad técnica.
- **PAKE (SRP/OPAQUE)**: el servidor no ve la password ni un equivalente. Es el camino si el modelo de amenaza se
  endurece (servidor hostil, no sólo comprometido). Requiere handshake propio + su ADR. **Queda anotada como evolución**,
  no descartada.

## Consecuencias

- `accounts.password_hash` (NOT NULL) y `auth.rs` suman hash/verificación con argon2id + `signin`.
- El bootstrap local de `accounts` (Fase 1) tiene que pedir la password: sin registro público, la creación de cuentas es
  una operación de arranque, no un endpoint abierto.
- Un cambio de password de cuenta **no toca** el contenido (no es una reenvoltura de `vault_key`): son conceptos
  distintos, y confundirlos sería el bug más caro de esta ADR.

# ADR 0015 — Autenticación del WebSocket para clientes que no pueden fijar headers

- **Fecha:** 2026-10-17
- **Estado:** aceptado
- **Origen:** al construir el núcleo durable (fase 2) apareció que el upgrade del WS exige
  `Authorization: Bearer`, y **ni el runtime del núcleo (Chromium) ni el plugin de Obsidian
  (renderer de Electron) pueden fijar headers en `new WebSocket()`**. Sin esto, el cliente TS
  no tiene ningún camino para el commit (que va SOLO por WS).

## Contexto

El único camino público del WebSocket (`/api/v1/ws`) exige identidad **en el upgrade**
(`routes::Autenticado` → `Authorization: Bearer <token de device>`) para fijar
`account_id`/`device_id` de la conexión antes del handshake de datos.

La spec congelada (`docs/protocol.md`, sección Datos) **no exige** ese header: define el
handshake `hello → hello_ok → auth (proof) → auth_ok`, que prueba posesión de la **clave del
vault** (Ed25519) sin mandarla. El header es una capa de identidad adicional de la
implementación, no un requisito del contrato.

La WebSocket API del navegador permite exactamente dos cosas para autenticar el upgrade:
`Sec-WebSocket-Protocol` (subprotocolos) y la URL. Los headers arbitrarios no existen.
Poner el token en la URL lo hace aparecer en logs de proxies (rompe la regla "el token no se
loguea jamás") y en historiales.

## Decisión

El upgrade acepta el token por **dos vías equivalentes**:

1. `Authorization: Bearer <token>` — como hasta ahora (clientes con control de headers:
   smoke-cliente, curl, futuros móviles nativos).
2. Subprotocolo: `Sec-WebSocket-Protocol: ovaultd.v1, ovaultd.bearer.<token en base64url
   sin padding>` — el navegador puede ofrecer subprotocolos aunque no headers.

El server **ecoa `ovaultd.v1`** (RFC 6455: si el cliente ofreció subprotocolos, la respuesta
debe incluir uno de ellos o el navegador corta la conexión) y **nunca ecoa el bearer**.

El bearer del subprotocolo tiene que ser el token y decodificarse exactamente igual que el
header: una sola fuente de autenticación (`routes::autenticar_token`).

## Consecuencias

- `sin_token_no_hay_handshake` sigue valiendo: sin header NI subprotocolo, el upgrade es 401.
- El token viaja siempre en un **header**, nunca en la URL: proxies y logs quedan limpios.
- Un cliente que no ofrece subprotocolos (el smoke en Rust) no ve cambios.
- El challenge/proof del handshake de datos queda intacto: esto es identidad de conexión,
  no reemplaza la prueba de la clave.
- Clientes futuros (móvil) eligen la vía que les sirva; la superficie del contrato no cambia.

## Alternativas descartadas

- **Token en query string** (`?token=`): aparece en logs de acceso de proxies y servidores,
  contra la regla explícita del proyecto.
- **Ticket efímero por HTTP** (`POST /ws-ticket` → ticket de un uso): correcto pero agrega
  estado de servidor (emisión, expiración, consumo) para un problema que el subprotocolo
  resuelve sin estado extra.
- **Autenticar solo in-band** (postergar la identidad al proof): dejaría sockets anónimos
  abiertos aceptando mensajes, ampliando la superficie de DoS sin necesidad.

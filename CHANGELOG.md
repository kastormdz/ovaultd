# Changelog

Todos los cambios relevantes de este proyecto, por corte de release.
El formato sigue [Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/) y el versionado es
[semver](https://semver.org/lang/es/): server y plugin viajan en un **release train** (mismo
`MAJOR.MINOR` = pareja probada junta; ver ADR-0018 y la sección «Compatibilidad» del README).

## [0.2.2] — 2026-10-18

Parche de seguridad sobre la 0.2.1 (primer bloque del plan de remediación de la auditoría del
2026-10-18).

### Fixed

- **Freno de fuerza bruta en el API de login**: `POST /api/v1/auth/signin` no tenía límite (solo
  el panel web lo tenía). Ahora usa el mismo freno (5 intentos por minuto y por `ip|email`, `429`
  con `Retry-After`, auditado), y los intentos contra cuentas inexistentes también cuentan: el
  password spraying y la quema de CPU por Argon2 quedan cortados sin afectar los usos legítimos
  (el plugin y el CLI hacen signin de baja frecuencia).
- **Los headers de un proxy ya no son de cualquiera**: `X-Real-IP` / `X-Forwarded-For` /
  `X-Forwarded-Proto` se obedecen SOLO con el nuevo `OVAULTD_TRUST_PROXY=1` (default `0`); sin el
  flag, la IP del cliente sale del peer TCP real y el `Secure` de la cookie no depende de un
  header. Antes, rotar `X-Forwarded-For` esquivaba el freno de login y un
  `X-Forwarded-Proto: http` abría la cookie del panel.

  **Si el server está detrás de un borde (NPM/nginx/Caddy), activá `OVAULTD_TRUST_PROXY=1`**
  (ver `docs/deploy.md`): sin el flag el freno sigue funcionando por cuenta, pero pierde la IP
  real del cliente y la cookie queda conservadora (sin `Secure`).

## [0.2.1] — 2026-10-18

Parche de corrección sobre la 0.2.0, más la primera pieza del sync en vivo: el server avisa a los
demás dispositivos del vault apenas alguien commitea.

### Added

- **Aviso entre dispositivos (`notify` por WebSocket)**: al commitear un lote, el server reenvía el
  registro a las demás conexiones del mismo vault. El aviso sale del oplog recién commiteado, así
  que no puede mentir sobre lo que hay, y es **sólo una señal**: quien lo recibe hace un pull con su
  `since`, de modo que un aviso perdido no pierde ningún dato. Cubierto por un test con dos
  dispositivos reales del mismo vault.

### Fixed

- **`/setup` sin estilos**: la pantalla de la primera cuenta se armaba como HTML a mano, con estilos
  inline, y salía pelada (ni siquiera pedía el CSS del panel). Ahora usa la plantilla
  `base_centrado.html`, la misma de login y registro, y sus textos viven en `admin/textos.rs`, como
  el resto de la interfaz.
- **Los botones del panel tenían el texto pegado a la izquierda**: `.boton` es `inline-flex` y no
  declaraba `justify-content`, así que el `text-align` del navegador no aplicaba.
- **Verificación de leaks**: la etiqueta de interfaz «Password de la cuenta» se veta por nombre,
  como el resto del glosario de textos de UI.

## [0.2.0] — 2026-10-17

Primera release publicada: el server sale del «funciona en mi lab» y pasa a tener panel de
administración, contrato de compatibilidad y distribución por releases.

### Added

- **Panel web de administración** (server-rendered, sin CDNs ni Node; funciona sin JavaScript):
  - Cuentas con roles: crear, deshabilitar/habilitar, promover/degradar, links de
    restablecimiento de un solo uso (24 h) y borrado con purga de sus vaults.
  - Vaults: listado con contadores por vault, detalle (dueño, `enc_version`, generación de
    llave, cuota, notas, revisiones, lotes, fragmentos, bytes, miembros, últimos lotes) y
    borrado con confirmación por **nombre exacto**.
  - Dispositivos: listado global con tokens vivos y última actividad, y revocación instantánea
    (mata el dispositivo y todos sus tokens).
  - Auditoría paginada con filtro por acción o detalle.
  - Ajustes: abrir/cerrar el registro público. Cambio de password propio.
- **`GET /api/v1/meta`** (público): contrato de compatibilidad con `server_version`,
  `api`, `wire_protocol`, `min_plugin_version` y `features`.
- **Borrado de vault por API** (`DELETE /api/v1/vaults/{id}?confirm=<nombre>`, con cascada y
  barrido del CAS) y **re-sellado de material** (`PUT /api/v1/vaults/{id}/material`).
- Cabeceras de seguridad en todo el panel: CSP estricta (`default-src 'none'`), `nosniff`,
  `referrer-policy`, `permissions-policy` y `frame-ancestors 'none'`.
- CORS: se permite el header `x-ovaultd-client` con el que los clientes se identifican.

### Changed

- **Diseño del panel**: «command center» con rail lateral, veredicto de salud del servidor
  (uptime, tamaño de la base, almacén cifrado, última sincronización), contadores, últimas
  acciones y estados vacíos. Todo el texto visible vive en un módulo de textos (`textos.rs`),
  listo para un segundo idioma sin tocar plantillas.
- Los assets del panel se sirven con `ETag` + `no-cache` (y URL versionada al cambiar el
  diseño): un HTML nuevo contra una hoja cacheada dejaba el panel sin estilo.
- Responsive real: el rail envuelve en pantallas chicas y las tablas anchas se deslizan dentro
  de su tarjeta.
- El panel muestra solo lo accionable: los parámetros de arranque (que se configuran por
  entorno) salieron de «Ajustes».

### Fixed

- Mensajes del panel con acentos: iban pre-codificados a mano y se re-codificaban al doble
  (`+` literal en pantalla).
- La guarda del `apply` compara contra el hash del contenido descifrado: un bloqueo en falso
  de un local que ya coincidía con el remoto ahora converge.

### Security

- El server sigue siendo **ciego**: nunca recibe `vault_key`, KEK ni recovery, y no descifra
  contenido. El panel administra cuentas y metadatos, no llaves del vault.

[0.2.0]: https://github.com/kastormdz/ovaultd/releases/tag/server-v0.2.0

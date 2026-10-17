# Plan — Panel web de administración, versionado y borde

- Estado: **en ejecución** — **PD ✅, W0 ✅, W1 ✅, W2 ✅, W3 ✅ y V1 ✅** (2026-10-17); las 4 decisiones se
  confirmaron tal cual (stack server-rendered, registro cerrado + toggle, borrado con bloques,
  español). Sigue: **V2** (releases en GitHub).
- Origen: pedido directo del operador (2026-10-17): el admin tiene que ser **web, moderno y
  PRO** (cuentas, passwords, stats, registro on/off); **sin proxy en el docker** (el borde lo
  pone NPM Plus, que ya opera); **versiones explícitas** server ↔ plugin y **releases en
  GitHub**.
- ADRs: [`0017`](decisions/0017-panel-web-de-administracion.md) (panel) ·
  [`0018`](decisions/0018-versionado-y-compatibilidad.md) (versiones/compat/releases) ·
  [`0019`](decisions/0019-el-borde-es-del-operador.md) (borde).

## Decisiones propuestas (confirmar antes de arrancar)

1. **Stack del panel: server-rendered con askama + htmx + CSS propio** (recomendado) —
   binario único, sin toolchain Node en el server, rápido y auditable. Alternativa: SPA
   (React/Vue) con build aparte — más peso, más piezas; no la recomiendo para este proyecto.
2. **Registro cerrado por defecto + toggle en `/admin/ajustes`** — sigue la línea de
   ADR-0011/0016: `/register` existe solo si el admin lo prende. Alta directa por el admin y
   reset por **token de un uso mostrado una vez** (sin SMTP).
3. **Borrar cuenta con vaults**: bloquear y pedir borrar primero los vaults del owner (con
   `force` explícito como escape). Nunca se borra el último admin.
4. **Idioma del panel: español** (coherente con `/setup`); i18n cuando haya público.

## Cortes (uno por sesión, con gate completo antes de commit)

### PD — Borde: fuera el proxy del repo (ADR-0019) — chico, primero ✅

- ✅ `docker/proxy/` eliminado; el overlay de dev solo construye la imagen; `container-e2e.sh`
  prueba DIRECTO contra el server (healthz, `/metrics`, smoke completo HTTP+WS con cripto real,
  sync E2E de Chromium, persistencia con recreate). Gate corrido: CONTAINER-E2E OK.
- ✅ `docs/deploy.md` reescrito: NPM Plus primero (los 3 puntos + cómo lo alcanza un borde en
  contenedor), nginx de referencia, Caddy. READMEs sin OVAULTD_PROXY_PORT.
- Commit `db72115` (pusheado).

### W0 — Cimientos del panel (ADR-0017) ✅

- ✅ Migración `005_admin.sql`: `accounts.role` (bootstrap nace `admin`; migración promueve a
  `MIN(id)` en servidores ya bootstrapeados), `sessions` (cookie opaca con hash + CSRF crudo),
  `audit_log`, `server_settings` (`registro_abierto=off`).
- ✅ Módulo `admin/`: stack askama 0.14 (templates dentro del binario) + htmx 2.0.4 vendorizado
  + CSS propio dark, sin CDNs · login/logout con cookie (`HttpOnly`, `SameSite=Lax`, `Secure`
  automático por `X-Forwarded-Proto` con `OVAULTD_SECURE_COOKIES=auto|on|off`) · CSRF por sesión
  en TODOS los POST (tiempo constante) · freno de 5 intentos/min por ip+email (429+Retry-After)
  · auditoría de cada intento · `GET /` → `/admin` con cuentas · `/admin/mi-cuenta` (cambia la
  password propia y revoca las demás sesiones).
- ✅ Tests: 9 de integración (`tests/admin.rs`) + unit (sesiones/limitador/config) —
  workspace **170 passed / 0 failed** + SMOKE + CLI-E2E + CONTAINER-E2E verdes. Prueba viva
  sobre el binario: bootstrap → login → dashboard → auditoría en la DB. Plan de W1 continúa
  abajo sin cambios.

### W1 — Cuentas, registro y reset ✅

- ✅ Migración `006_w1_cuentas.sql`: `accounts.disabled_at` + `password_resets` (`token_hash`
  SHA-256, vence a las 24 h, un solo uso — sin SMTP: el link lo ve el admin UNA vez).
- ✅ `/admin/cuentas` (solo rol `admin`; sin rol → 403 con página propia, sin redirect):
  listar paginado (25/pág con nº de vaults y última actividad), crear, deshabilitar/habilitar
  (deshabilitar mata la sesión web al instante — y los tokens de API/WS mueren por el JOIN
  nuevo en `autenticar_token`, la fuente única HTTP+WS), promover/degradar, reset y borrar
  (confirmación por email EXACTO + purge de vaults con checkbox explícito, reusa
  `purgar_vault`). Invariables: nunca el último admin activo, nunca la propia cuenta. Todo
  auditado.
- ✅ `/register` (solo con el registro abierto; freno por IP sobre el limitador del login) y
  `/reset?token=…` (un solo uso: al cambiar la password mueren TODAS las sesiones web + los
  tokens de devices, y el token queda quemado).
- ✅ `/admin/ajustes`: toggle de registro (nace `off`, ADR-0011) + parámetros read-only del
  server. El dashboard suma la sección «Administración» para admins.
- ✅ Gate: workspace **182 passed / 0 failed** (7 tests de integración nuevos en
  `tests/admin.rs`) + E2E vivo sobre el binario (`:8896`): bootstrap → login → crear →
  deshabilitar/habilitar → reset (link una vez) → registro por toggle → borrado, con la
  auditoría completa en la DB.
- Commit `964bd29` (pusheado).

### W2 — Vaults, devices y auditoría ✅

- ✅ `/admin/vaults`: cada fila suma [Detalle] [Borrar…]. El detalle muestra TODO lo que el
  server puede ver (dueño, `enc_version`, `head_rev`, epoch, cuota, notas vivas, revisiones,
  batches, blobs, bytes, miembros y los últimos 10 batches con su device); el borrado es la
  MISMA operación que la API (`purgar_vault`: cascada + barrido del CAS) con confirmación por
  NOMBRE EXACTO y auditoría.
- ✅ `/admin/devices` (global): cuenta, nombre, plataforma, estado, tokens vivos, última
  actividad (`COALESCE(last_seen, último batch)`) y revocación con un click
  (`auth::revocar_device`: marca el device y TODOS sus tokens; el token muerto deja de andar
  en el mismo request).
- ✅ `/admin/auditoria`: paginada (50/pág) con filtro por subcadena sobre acción o detalle.
- ✅ Las altas de vault ahora también se auditan (`vault_created` en `POST /api/v1/vaults`).
- ✅ Gate: workspace **187 passed / 0 failed** (5 tests nuevos en `tests/admin.rs`: detalle
  con los números de la DB, borrado por nombre + purge + audit, revocar mata el token de
  verdad, lista + filtro de auditoría, 403 sin rol en las 4 páginas nuevas) + E2E vivo contra
  el lab (`:8894`): vault real creado y sincronizado por CLI (3 notas), números del panel ==
  DB, borrado desde la web (rebote con nombre mal y purga con el exacto), revocación de
  device en vivo y `vault_created` visible en la bitácora.
- Commit `a98962e` (pusheado).

### W3 — Dashboard y pulido PRO

- Dashboard con contadores, versión del server, estado (uptime, CAS), últimas acciones;
  empty states; responsive; CSP estricta + `no-store`; microcopy en español.
- **Gate**: specs browser de las páginas clave + capturas en CI (artefactos).

**Cerrado (2026-10-17):** el dashboard ya traía contadores, uptime, CAS (fragmentos + bytes) y
las últimas acciones, con empty states en las tres listas. Lo que agregó W3:

- **CSP estricta** en todas las páginas del panel (`default-src 'none'; script-src 'self';
  style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'`) más
  `nosniff`, `referrer-policy: no-referrer`, `permissions-policy` y `x-frame-options: DENY`,
  aplicadas en el punto único que emite respuestas (`con_seguridad`). Para que la CSP fuera
  estricta de verdad hubo que sacar los 22 `style=""` inline (ahora clases utilitarias en
  `admin.css`) y desactivar por configuración declarativa (`<meta name="htmx-config">`) el
  `<style>` de indicadores que htmx inyecta solo.
- **Responsive real**: el rail en móvil no envolvía y sus links en fila forzaban el ancho de
  toda la página (571 px en un viewport de 390). Ahora envuelve, hay breakpoint de 620 px, y
  las tablas anchas se deslizan dentro de su tarjeta con barra de scroll visible (en dark una
  sombra sobre casi-negro no se percibe: la pista es la barra misma).
- **Gate nuevo**: `tests/panel-audit.mjs` — 10 páginas × 2 viewports (1440 y 390) con login
  por formulario real; falla si hay status ≠ 200, desborde horizontal, desborde interno sin
  scroll propio, error de consola (ahí aparecen las violaciones de CSP) o request fallido; y
  deja capturas de escritorio y móvil. Requiere `PANEL_PASSWORD` por entorno: las credenciales
  del lab no viven en el repo.

### V1 — Meta, versiones y compat (ADR-0018)

- `GET /api/v1/meta` con `{server_version, api, wire_protocol, min_plugin_version, features}`.
- Header `x-ovaultd-client` en los clientes; check de `/meta` al vincular/conectar en el
  plugin (mensaje claro si el plugin es viejo); server `0.2.0` · plugin `0.2.0` (release
  train); matriz de compatibilidad en los READMEs.
- **Gate**: tests del meta + spec del check del plugin + bump de versiones verificado por
  preflight.

**Cerrado (2026-10-17):** server y plugin en `0.2.0` (release train) con el contrato vivo:

- `GET /api/v1/meta` público (sin auth): `{server_version, api, wire_protocol,
  min_plugin_version, features}`. `server_version` sale de `env!("CARGO_PKG_VERSION")` (no de
  una copia a mano) y el wire/API de consts en `ovaultd-protocol`. Features anunciadas:
  `meta`, `admin-web`, `resumable-uploads`, `vault-delete`, `vault-material`, `device-revoke`.
- El CORS del server permite el header `x-ovaultd-client` (sin eso el preflight lo corta y el
  plugin no llega ni a vincular). Todo pedido del plugin lo manda: `oVaultd-plugin/<version>`.
- Plugin `0.2.0`: `src/compat.ts` (puro y testeado) decide con `evaluarCompatibilidad`;
  consulta `/meta` **antes de vincular y de crear** un vault (bloqueo con mensaje que apunta al
  plugin, nunca al server), al arrancar el motor (no sincroniza nada si es incompatible) y en
  «Probar conexión» (que ahora dice el veredicto, no solo «responde»).
- Matriz de compatibilidad en los tres READMEs (server EN/ES + plugin).
- Tests: `crates/ovaultd-server/tests/meta.rs` (3: público y completo, release train,
  preflight del header) + `test/compat.test.ts` (7) + los mocks de `crear-vault.test.ts`
  ahora sirven `/meta` como el server real.

### V2 — Releases en GitHub

- `.github/workflows/release-server.yml` (tag `server-v*`): gates → binario musl + tar.gz +
  SHA-256 → imagen `ghcr.io/kastormdz/ovaultd:X.Y.Z` y `:latest` → GH Release con assets.
- `release-plugin.yml` (tag `plugin-v*`): build → zip `main.js`/`manifest.json`/`styles.css`
  → GH Release (listo para BRAT/tienda).
- `CHANGELOG.md` en ambos repos + sección "Releases" en READMEs.
- **Gate**: primera release real — `server-v0.2.0` y `plugin-v0.2.0` publicadas y
  verificadas (descargar el asset, correrlo).

## Orden y relación con el resto

```
PD (chico) → W0 → W1 → W2 → W3 → V1 → V2
```

- El corte **3c del plugin** (dos vaults reales) queda para después de PD (el stack de dev
  cambia) o intercalado si el operador lo pide antes.
- La fase 4/5 del plan grande (E2E+rotación+recovery, conflictos+history+restore) sigue
  vigente después de esta tanda.
- `ovaultd-admin` no se toca: pasa a ser el pie de emergencia.

## Qué NO entra acá

- Compartir vaults / miembros (fase 4/5, necesita re-envolver `vault_key`).
- SMTP (self-host sin mail; los tokens de reset se pasan a mano).
- Cambiar la password del VAULT desde el server (imposible por diseño: E2EE; irá como
  re-wrap en el cliente).
- i18n del panel, temas configurables, gráficos avanzados (sparklines sí, si salen gratis).

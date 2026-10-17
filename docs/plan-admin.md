# Plan — Panel web de administración, versionado y borde

- Estado: **propuesto** — esperando confirmación de las 4 decisiones marcadas abajo.
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

### PD — Borde: fuera el proxy del repo (ADR-0019) — chico, primero

- Borrar `docker/proxy/` y el servicio `proxy` del overlay dev; `docs/deploy.md` con la
  configuración **NPM Plus exacta** (los 3 puntos) + nginx genérico como alternativa.
- `container-e2e.sh`: probar directo (HTTP+WS+cripto), sin esperas de nginx.
- **Gate**: `container-e2e` OK sin proxy · server-smoke · scan-leaks.

### W0 — Cimientos del panel (ADR-0017)

- Migración `005_admin.sql`: `accounts.role` (+promote de `MIN(id)` a admin si hay cuentas),
  `sessions`, `audit_log`, `server_settings` (`registro_abierto=off`).
- Módulo `routes/admin/`: stack askama + htmx vendorizado + layout dark · login/logout con
  cookie + CSRF + rate limit · `GET /` nueva (landing según estado) · `/admin/mi-cuenta`
  (cambiar password propia: revoca las demás sesiones).
- **Gate**: tests de integración (como `setup.rs`): sesión expira, CSRF inválido → 403,
  rate limit → 429, último admin no se degrada, `/setup` intacto · npm/browser/cargo verdes.

### W1 — Cuentas, registro y reset

- `/admin/cuentas`: listar (paginado), crear, deshabilitar, borrar (confirmación por doble
  paso), promover/degradar rol.
- `/register` (solo con registro abierto) + `/reset?token=…` con token de un uso.
- `/admin/ajustes`: toggle de registro + parámetros del server.
- **Gate**: tests + specs de browser (Playwright del repo) sobre flujo completo:
  login → crear cuenta → reset → cambiar password → borrar.

### W2 — Vaults, devices y auditoría

- `/admin/vaults` con stats reales (notas, revisiones, blobs/bytes, última actividad) y
  detalle; borrar vault con confirmación fuerte. `/admin/devices` global + revocar +
  `last_seen`. `/admin/auditoria` paginada.
- **Gate**: tests + E2E contra una DB con vault real (el vault del container-e2e): los
  números del panel iguales a los de la DB.

### W3 — Dashboard y pulido PRO

- Dashboard con contadores, versión del server, estado (uptime, CAS), últimas acciones;
  empty states; responsive; CSP estricta + `no-store`; microcopy en español.
- **Gate**: specs browser de las páginas clave + capturas en CI (artefactos).

### V1 — Meta, versiones y compat (ADR-0018)

- `GET /api/v1/meta` con `{server_version, api, wire_protocol, min_plugin_version, features}`.
- Header `x-ovaultd-client` en los clientes; check de `/meta` al vincular/conectar en el
  plugin (mensaje claro si el plugin es viejo); server `0.2.0` · plugin `0.2.0` (release
  train); matriz de compatibilidad en los READMEs.
- **Gate**: tests del meta + spec del check del plugin + bump de versiones verificado por
  preflight.

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

# 0017 — Panel web de administración

- Estado: aceptada
- Fecha: 2026-10-17

## Contexto

ADR-0011 cerró el registro público y ADR-0016 agregó un bootstrap web que muere con la
primera cuenta: a partir de ahí, administrar un servidor ovaultd es tarea de CLI
(`ovaultd-admin`). Eso funciona para crear cuentas desde el host, pero deja afuera todo lo
demás que un operador necesita a diario: ver cuentas, revocar devices, mirar vaults y su
tamaño, resetear una password, cerrar el registro, auditar qué pasó. El pedido del operador
es explícito: **administración por web, moderna y presentable**, no comandos.

## Decisión

### Roles

- `accounts.role TEXT NOT NULL DEFAULT 'user'` (`'admin' | 'user'`). La cuenta del bootstrap
  (`/setup`, ADR-0016) nace `admin`; para servidores ya bootstrapeados, la migración promueve
  la cuenta más antigua (`MIN(id)`). Promover/degradar se hace desde el panel.
- Invariante: **el último admin no se degrada ni se borra** (chequeo en el handler, dentro de
  la transacción).

### Sesiones web (distintas de los tokens de device)

- Tabla `sessions`: `id`, `token_hash` (SHA-256 del token; el crudo solo viaja en la cookie),
  `csrf_token_hash`, `account_id`, `created_at`, `expires_at`, `last_seen`, `ip`, `user_agent`.
- Cookie `ovaultd_session`: `HttpOnly`, `SameSite=Lax`, `Path=/`, `Secure` automático cuando
  el request llega con `X-Forwarded-Proto: https` (NPM Plus lo setea), con override
  `OVAULTD_SECURE_COOKIES`. TTL 30 días con renovación deslizante.
- **CSRF**: token por sesión (campo oculto en cada formulario + doble verificación en POST);
  cualquier POST sin token válido es 403. `SameSite=Lax` como segunda capa.
- **Rate limit de login** en memoria (single-node por diseño): N intentos por IP+email en
  ventana deslizante; al exceder, 429 con `Retry-After`. Todo intento (ok o no) va a auditoría.
- Logout revoca la fila de `sessions`. Cambiar la password propia revoca TODAS las demás.

### Stack

- **HTML server-rendered con askama** (templates compile-time, cero runtime de templates) +
  **htmx embebido** (un único archivo vendorizado con `include_str!`, versión fijada) +
  CSS propio (dark, limpio, sin CDNs: el server no llama a internet). El panel funciona sin
  JS (formularios clásicos); htmx agrega refrescos parciales y confirmaciones, progresivo.
- Sin SPA y sin toolchain Node en el server: el binario sigue siendo único y auditable. El
  repo del server no gana un `package.json`.

### Superficie

| Ruta | Qué es |
|---|---|
| `/` | Landing: sin cuentas → `/setup`; con cuentas → login o dashboard |
| `/admin/login`, `/admin/logout` | Sesión web |
| `/admin` | Dashboard: contadores globales, versión, última actividad |
| `/admin/cuentas` + `/{id}` | Listar/crear/deshabilitar/borrar/promover/reset de cuentas |
| `/admin/vaults` + `/{id}` | Vaults con stats (notas, revisiones, storage), detalle, borrar |
| `/admin/devices` | Devices de todas las cuentas; revocar |
| `/admin/auditoria` | Log de acciones (paginado) |
| `/admin/ajustes` | Registro abierto/cerrado; ajustes del server |
| `/admin/mi-cuenta` | Cambiar la password propia (actual + nueva) |
| `/register` | ALTA de cuenta propia — existe SOLO con registro abierto |
| `/reset?token=…` | Cambio de password con token de un uso |

### Registro y altas

- Setting `registro_abierto` (tabla `server_settings`, default `off` — sigue cerrado como
  ADR-0011; el admin lo prende desde `/admin/ajustes`).
- Alta directa por el admin (email + password inicial, o mejor: cuenta + token de reset para
  que la persona elija su password — sin SMTP, el admin pasa el link por fuera).
- Reset de password: el admin genera un **token de un uso** que se muestra UNA vez (no hay
  mail); `/reset?token=…` deja fijar la password nueva. El admin nunca setea ni ve passwords.
- La password del **vault** no es asunto del panel (el server es ciego, ADR-0011): cambiar una
  password de vault es re-envolver material en el cliente (feature del plugin/CLI, fase 4/5).
  El panel lo dice explícito donde corresponda.

### Auditoría y stats

- `audit_log`: `id`, `ts`, `actor_account_id`, `action` (login_ok/login_fail/logout/
  cuenta_creada/cuenta_borrada/cuenta_deshabilitada/reset_emitido/device_revocado/
  vault_borrado/registro_toggle/rol_cambiado), `detail` (JSON), `ip`.
- Dashboard y detalle de vault: cuentas, vaults, notas (`files` no borrados), revisiones,
  blobs y bytes del CAS, devices activos, última actividad. Agregados SQL directos.

## Consecuencias

- `ovaultd-admin` queda como **herramienta de rescate** (pie de emergencia si la web no está
  accesible); la vida diaria pasa por el panel.
- El registro sigue cerrado por defecto: abrirlo es una decisión explícita del admin.
- El panel es la superficie más sensible del server: CSP estricta (`default-src 'self'`),
  `cache-control: no-store` en páginas autenticadas, rate limit, auditoría completa.
- Idioma del panel: español (coherente con `/setup`); i18n queda para cuando haya público.
- Las sesiones web NO son tokens de device y no sirven para el API ni el WS: son cookies de
  navegador para el panel; el API sigue token-based (los clientes no cambian).

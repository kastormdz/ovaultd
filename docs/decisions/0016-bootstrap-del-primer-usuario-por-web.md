# 0016 — Bootstrap del primer usuario por web (`/setup`)

- Estado: aceptada
- Fecha: 2026-10-17

## Contexto

ADR-0011 cerró el registro público: las cuentas se crean con `ovaultd-admin`, un CLI local
sobre la base que viene dentro de la imagen. Para operar un servidor ya andando eso está bien,
pero deja un hueco en el primer arranque: sin shell en el host no hay forma de crear la
PRIMERA cuenta. El patrón conocido para eso (Jellyfin, Gitea y compañía) es un formulario de
instalación que existe solo mientras el servidor está vacío y se desactiva para siempre al
crear la primera cuenta.

## Decisión

- `GET /` sin cuentas redirige a `GET /setup`; con cuentas, muestra una página mínima de
  identificación.
- `GET /setup` muestra el formulario SOLO si `SELECT COUNT(*) FROM accounts = 0`; con
  cualquier cuenta responde `410 Gone` para siempre (y `POST /setup` igual).
- `POST /setup` crea la primera cuenta (email + password de CUENTA; argon2id igual que el
  resto) y cierra el bootstrap. La condición "no hay ninguna" vive DENTRO del INSERT
  (`INSERT … SELECT … WHERE NOT EXISTS (…)`): SQLite serializa a los escritores, así que dos
  envíos simultáneos no pueden crear dos cuentas — el segundo afecta 0 filas y recibe el
  mismo 410.
- `OVAULTD_SETUP_TOKEN` (opcional): seteado, `/setup` exige `?token=…` (campo oculto en el
  POST). Cubre la ventana entre "el servidor quedó expuesto" y "terminé el bootstrap".
- La página es HTML mínimo embebido (sin JS ni frameworks), con `cache-control: no-store` y
  **sin interpolar nunca input del usuario** en el cuerpo (cero superficie de XSS).
- La cuenta creada por web es una cuenta de CONTROL (login + devices) como cualquier otra:
  las passwords de cifrado de vault siguen siendo del cliente y el servidor nunca las ve.
- Este ADR aplica SOLO al bootstrap. El alta de cuentas adicionales sigue siendo
  `ovaultd-admin` (ADR-0011), hasta que el sharing de vaults pida invitaciones (fase 4/5).

## Consecuencias

- El primer usuario ya no depende de una consola: `docker compose up` + abrir `/setup`.
- El registro sigue cerrado: `/setup` no es un endpoint de registro, es un bootstrap que
  muere con la primera cuenta.
- Un servidor expuesto y NO bootstrapeado, sin token, es "del primero que llega": el token
  opcional y la recomendación de bootstrapear ANTES de exponerlo cubren el caso.
- Un device nuevo de la misma persona NO necesita cuenta nueva (`login` suma un device); una
  cuenta nueva es para OTRA persona.

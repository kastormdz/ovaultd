# Despliegue — Docker y proxy reverso

> Guía operativa del empaquetado (fase 1). El contrato del server está en
> `docs/server-interfaces.md`; esto es cómo se lo pone a andar, y qué tiene que cumplir un proxy
> reverso que se ponga delante.

## Arranque (desarrollo / self-hosted)

```bash
cp .env.example .env
chmod 600 .env

docker compose -f compose.yaml -f docker/compose.dev.yaml up -d --build
curl --fail http://127.0.0.1:8080/healthz      # directo al server
curl --fail http://127.0.0.1:8888/healthz      # por el nginx de ejemplo
```

El overlay de desarrollo trae un **nginx con el mismo `docker/proxy/nginx.conf.example`** que
documenta esta guía: sirve para probar el stack completo detrás de un proxy sin montar nada.

Distribución: `OVAULTD_IMAGE=repositorio@sha256:<digest>` de una imagen **publicada real** (nunca
un digest inventado) y `docker compose -f compose.yaml up -d`, sin el overlay de desarrollo.

## Cuentas

No hay registro público (ADR-0011). La **primera** cuenta se crea por la web en
`http://<server>/setup`: solo existe mientras el servidor no tenga ninguna y se desactiva sola
al crearla (con `OVAULTD_SETUP_TOKEN` seteado además exige `?token=…` — útil si el server va a
quedar expuesto antes de terminar el bootstrap; ver ADR-0016). Las cuentas **siguientes** se
crean con `ovaultd-admin`, que viene **dentro de la imagen**:

```bash
printf '%s\n' '<password>' | docker compose exec -T ovaultd \
  ovaultd-admin create-account --email vos@ejemplo.com
```

La password se lee **siempre** de stdin (nunca por argumento). `ovaultd-admin` también
responde `health` (SELECT 1 contra la base) e `inspect` (conteos y tamaños; nada de esto
descifra contenido). Para operar contra una base que no está corriendo en el contenedor,
acepta `--db <ruta>` (o la variable `OVAULTD_DB`).

Ojo con el modelo: una cuenta nueva es para **otra persona**. Un dispositivo nuevo (otra
laptop, otro celular) se suma con `login` sobre la MISMA cuenta — no se crean cuentas por
dispositivo.

## Qué lleva el contenedor

- **Usuario non-root** (uid/gid 10001), **rootfs read-only**, `cap_drop: ALL`,
  `no-new-privileges`, límites de memoria/CPU y **healthcheck** contra `/healthz` (el mismo que
  usa la aceptación de la fase).
- Todo el estado bajo **`/data`** (volumen): `metadata.db` (SQLite WAL) + `cas/` (blobs) +
  `.uploads/` (temporales de subidas reanudables). El binario **embebe las migraciones**: migrar
  un volumen existente es automático al arrancar.
- **Logs JSON** (`OVAULTD_LOG_JSON=1`), nivel por `OVAULTD_LOG`. Ni tokens ni payloads se
  loguean jamás (regla del contrato).
- Superficie de operación: **`/healthz`** (200/503) y **`/metrics`** (Prometheus mínimo, sin
  secretos). Si no querés `/metrics` público, restringilo en el proxy (abajo).

## Proxy reverso — los tres puntos que no se pueden perder

El ejemplo completo y comentado está en **`docker/proxy/nginx.conf.example`**. Resumen:

1. **Host**: `POST /access` devuelve el `Host` que **ve el server** (`{host, rev}`), sin campo de
   URL pública en la config. El proxy **tiene que preservarlo** (`proxy_set_header Host $host;`
   en nginx; en Caddy/traefik/NPM es el comportamiento por default). Si el proxy lo reescribe,
   los clientes guardan una URL interna que después no resuelve.
2. **WebSocket** (`GET /api/v1/ws`): passthrough de `Upgrade`/`Connection` y timeouts de vida
   larga (`proxy_read_timeout 1h`): la conexión queda abierta escuchando revisiones.
3. **Tamaño y streaming**: `client_max_body_size 40m` (los bloques de la subida reanudable
   llegan hasta `MAX_FILE_BYTES` = 32 MiB; el default de nginx, 1 MB, los corta con 413) y
   `proxy_request_buffering off` para que los `PATCH` hagan streaming.

Extras que conviene setear: `X-Real-IP`/`X-Forwarded-For` (para la auditoría del proxy),
`X-Forwarded-Proto $scheme` (informativo; el server no lo consume), y **TLS terminado en el
proxy** — el server habla HTTP plano y no tiene TLS propio; no lo expongas directo a internet.

Con NPM (Nginx Proxy Manager): activar **Websockets Support**, subir *Client Max Body Size* en
Advanced (`client_max_body_size 40m;`) y desactivar el buffering (`proxy_request_buffering off;`).
Con Caddy: `reverse_proxy ovaultd:8080` ya preserva Host y hace WebSocket; agregar
`request_body { max_size 40MB }` no es necesario (Caddy no limita por defecto).

Aviso de seguridad: `/metrics` no lleva auth. En el ejemplo va restringido a redes privadas;
si tu proxy está en internet, dejalo para la red interna (o borralo del proxy).

## Backups

El par **DB + CAS** se respalda junto (el dump de la base sin los blobs no restaura nada):

```bash
# con el contenedor corriendo (SQLite en WAL hace checkpoint con .backup):
docker compose exec ovaultd sh -c 'command -v sqlite3 >/dev/null' # no viene; usá el host sobre el volumen:
# bind mount (OVAULTD_DATA):    sqlite3 "$OVAULTD_DATA/metadata.db" ".backup '/backups/metadata.db'"
#                               rsync -a "$OVAULTD_DATA/cas" /backups/
# volumen nombrado:             docker run --rm -v ovaultd-datos:/data -v "$PWD:/backup" debian \
#                                 sh -c 'cp -a /data /backup/ovaultd-datos-$(date +%F)'
```

El restore se verifica (levanta el contenedor sobre el par copiado y `/healthz` + un pull prueban
que la base y los blobs están consistentes).

## Problemas comunes

- **`unable to open database file` al arrancar sobre un volumen viejo**: los volúmenes
  creados por builds anteriores al fix tenían `/data` con dueño `root`, y el server corre
  como uid 10001. Si el volumen es desechable, borralo y que se recree (los nuevos copian
  el dueño correcto de la imagen). Si tiene datos:
  `docker run --rm -v ovaultd-datos:/data alpine chown -R 10001:10001 /data`.

## E2E del empaquetado

```bash
bash tests/scripts/container-e2e.sh
```

Levanta el stack (dev overlay + nginx), espera el healthcheck de Docker, corre el **smoke
completo a través del proxy** (HTTP + WebSocket, cripto real) y verifica persistencia con un
recreate. Los artefactos quedan en `.tmp/container-e2e/`.

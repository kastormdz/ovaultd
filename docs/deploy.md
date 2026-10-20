# Despliegue: Docker y borde reverso

> Guía operativa del empaquetado (fase 1). El contrato del server está en
> `docs/server-interfaces.md`; esto es cómo se lo pone a andar, y qué tiene que cumplir el
> borde que se ponga delante. El repositorio no trae proxy: el TLS y la terminación HTTP
> son del operador (ADR-0019): Nginx Proxy Manager, nginx o Caddy.

## Arranque (desarrollo / self-hosted)

```bash
cp .env.example .env
chmod 600 .env

docker compose -f compose.yaml -f docker/compose.dev.yaml up -d --build
curl --fail http://127.0.0.1:8080/healthz      # directo al server
```

El overlay de desarrollo solo construye la imagen local: el server publica en loopback por
defecto y no necesita proxy para funcionar.

Distribución: `OVAULTD_IMAGE=repositorio@sha256:<digest>` de una imagen publicada real (nunca
un digest inventado) y `docker compose -f compose.yaml up -d`, sin el overlay de desarrollo.

Cada release publica la MISMA imagen que pasó el E2E (se re-taggea, no se reconstruye) en
`ghcr.io/kastormdz/ovaultd`, con el tag de la versión (`X.Y.Z`, sin el prefijo `server-v`, que es
el tag de git) y `latest`. El digest a pinear es el del **manifest** (`docker manifest inspect` o
`docker inspect --format '{{index .RepoDigests 0}}'` tras un pull por tag), no el `{{.Id}}` que
imprime el log.

### Dos cosas que rompen en la primera puesta en producción

- Bind mount: el contenedor corre como `10001`. Con volumen nombrado no hay problema, pero un
  directorio que creás vos pertenece a tu uid → SQLite falla con `unable to open database file`
  (code 14) y el contenedor entra en restart-loop. Solución: `OVAULTD_UID`/`OVAULTD_GID` con tu
  `id -u`/`id -g`.
- Borde en OTRO contenedor: `OVAULTD_ADDR=127.0.0.1` publica en el loopback del host, y un
  contenedor (Nginx Proxy Manager, Caddy en Docker) **no puede alcanzarlo**: el proxy devuelve
  502 aunque el TLS y el redirect estén perfectos. Usá `OVAULTD_ADDR=0.0.0.0` (el server no
  trae TLS: que el puerto lo cierre el firewall del host, no el loopback).

## Cuentas

No hay registro público (ADR-0011). La **primera** cuenta se crea por la web en
`http://<server>/setup`: solo existe mientras el servidor no tenga ninguna y se desactiva sola
al crearla (con `OVAULTD_SETUP_TOKEN` seteado además exige `?token=…`, útil si el server va a
quedar expuesto antes de terminar el bootstrap; ver ADR-0016). Esa cuenta nace admin y su
casa es el panel web (`/admin`, ADR-0017): login con la password de la cuenta, dashboard y
cambio de password. Las cuentas **siguientes** se
crean con `ovaultd-admin`, que viene dentro de la imagen:

```bash
printf '%s\n' '<password>' | docker compose exec -T ovaultd \
  ovaultd-admin create-account --email vos@ejemplo.com
```

La password se lee siempre de stdin (nunca por argumento). `ovaultd-admin` también
responde `health` (SELECT 1 contra la base) e `inspect` (conteos y tamaños; nada de esto
descifra contenido). Para operar contra una base que no está corriendo en el contenedor,
acepta `--db <ruta>` (o la variable `OVAULTD_DB`).

Ojo con el modelo: una cuenta nueva es para **otra persona**. Un dispositivo nuevo (otra
laptop, otro celular) se suma con `login` sobre la MISMA cuenta; no se crean cuentas por
dispositivo.

## Qué lleva el contenedor

- Usuario non-root (uid/gid 10001), rootfs read-only, `cap_drop: ALL`,
  `no-new-privileges`, límites de memoria/CPU y healthcheck contra `/healthz` (el mismo que
  usa la aceptación de la fase).
- Todo el estado bajo `/data` (volumen): `metadata.db` (SQLite WAL) + `cas/` (blobs) +
  `.uploads/` (temporales de subidas reanudables). El binario **embebe las migraciones**: migrar
  un volumen existente es automático al arrancar.
- Logs JSON (`OVAULTD_LOG_JSON=1`), nivel por `OVAULTD_LOG`. Ni tokens ni payloads se
  loguean jamás (regla del contrato).
- Superficie de operación: `/healthz` (200/503) y `/metrics` (Prometheus mínimo, sin
  secretos). Si no querés `/metrics` público, restringilo en el borde (abajo).

## El borde (TLS + proxy reverso)

El server habla **HTTP plano** y no tiene TLS propio: el TLS lo termina el borde. No lo
expongas directo a internet.

### Cómo hace el borde para VER al server

Dos formas, ambas válidas:

- Misma red Docker (recomendado cuando el borde es un contenedor, como NPM Plus): poné el
  contenedor de ovaultd y el del borde en la misma red y usá **`ovaultd:8080`** como upstream.
  El puerto no hace falta publicarlo al host:

  ```bash
  # red compartida (ej.): cualquiera de las dos puntas puede crearla
  docker network create ovaultd-borde 2>/dev/null || true
  docker network connect ovaultd-borde ovaultd
  docker network connect ovaultd-borde <contenedor-del-borde>
  ```

  (En NPM Plus: *Forward Hostname* `ovaultd`, *Forward Port* `8080`.)

- Puerto en la LAN del host: `OVAULTD_ADDR=192.168.x.x docker compose up -d` y el borde
  apunta a `192.168.x.x:8080`. Solo con firewall: el server no tiene TLS. (Ojo: un borde en
  OTRO contenedor NO ve el `127.0.0.1` del host; este modo es para borde en otra máquina.)

### Nginx Proxy Manager (NPM / NPM Plus)

1. *Proxy Host* → Forward Hostname / IP: `ovaultd` (misma red) o la IP del host;
   Forward Port: `8080`; *Scheme*: `http`.
2. *Details*: Websockets Support → ON (el WS de `/api/v1/ws` es la mitad del protocolo).
3. *Advanced* (custom config):

   ```nginx
   client_max_body_size 40m;        # subidas reanudables hasta 32 MiB (el default 1m corta con 413)
   proxy_request_buffering off;     # los PATCH de subida hacen streaming
   ```

4. *SSL*: el certificado de la pestaña SSL (Let's Encrypt o propio). NPM ya preserva el
   `Host` y setea `X-Forwarded-Proto` por default.
5. `OVAULTD_TRUST_PROXY=1` en el entorno del server (compose/`.env`): el server solo
   obedece los headers `X-Forwarded-*` con ese flag (default `0`); sin él, la IP del freno
   de login y el `Secure` de la cookie quedan en el modo conservador.

### Los cuatro puntos que cualquier borde tiene que respetar

1. Preservar el `Host` (o fijarlo con `OVAULTD_PUBLIC_HOST`): `POST /access` devuelve el
   host que **ve el server** (`{host, rev}`) y los clientes lo usan para subir. Con
   `OVAULTD_PUBLIC_HOST=obs.ejemplo.com` el server devuelve SIEMPRE ese valor (recomendado
   detrás de un borde: el `Host` del request es spoofeable y el cliente persiste lo que
   aprende). Sin la variable, el proxy tiene que preservarlo (`proxy_set_header Host $host;`
   en nginx; default en NPM/Caddy/traefik) o las URLs guardadas no resuelven.
2. WebSocket (`GET /api/v1/ws`): passthrough de `Upgrade`/`Connection` y timeouts de vida
   larga (`proxy_read_timeout 1h`): la conexión queda abierta escuchando revisiones.
3. Tamaño y streaming: `client_max_body_size 40m` y `proxy_request_buffering off` (los
   bloques llegan hasta `MAX_FILE_BYTES` = 32 MiB; el default de nginx, 1 MB, los corta con 413).
4. Los headers de identidad (`X-Real-IP`, `X-Forwarded-For` y `X-Forwarded-Proto`; el
   ejemplo de abajo ya los setea): se obedecen SOLO con `OVAULTD_TRUST_PROXY=1`. Activalo
   únicamente si el server está detrás de tu borde: sin borde, esos headers los puede mandar
   cualquiera y falsearían la IP del freno de login y el `Secure` de la cookie. El borde
   tiene que pisarlos SIEMPRE (el ejemplo usa `$remote_addr`, que ignora los del cliente).
   Ojo con NPM Plus: su `real_ip` (activo por defecto) resuelve la IP del
   `X-Forwarded-For` cuando el peer cae en una red "confiable" (`set_real_ip_from`: redes
   privadas + rangos de Cloudflare). Es el modelo esperado del borde: los requests de esas
   redes pueden decir su IP y el server la obedece; para un peer de internet la IP queda
   real y el XFF del atacante viaja como `<basura>, <ip-real>`: el server toma la última y
   el freno no se puede falsificar desde afuera. Verificación sin salir del host: forzar el
   peer por una interfaz fuera de esas listas (p.ej. Tailscale, 100.64/10) y comprobar que
   la auditoría registra la IP del peer y no el header.

nginx mínimo de referencia:

```nginx
location / {
    proxy_pass http://ovaultd:8080;
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 1h;
    client_max_body_size 40m;
    proxy_request_buffering off;
}
```

Con Caddy: `reverse_proxy ovaultd:8080` ya preserva Host y hace WebSocket; Caddy no limita el
body por defecto (no hace falta `request_body`).

`/metrics`: la ruta sale de `OVAULTD_METRICS`: `off` por defecto (no existe: 404),
`localhost` (solo el peer de loopback) u `on` (expuesta como el resto del API). Con `on`,
restringila en el borde:

```nginx
location /metrics {
    deny all;    # 403; si preferís ni revelar que existe: `return 404;`
}
```

## Backups

El par DB + CAS se respalda junto (el dump de la base sin los blobs no restaura nada):

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

- `unable to open database file` al arrancar sobre un volumen viejo: los volúmenes
  creados por builds anteriores al fix tenían `/data` con dueño `root`, y el server corre
  como uid 10001. Si el volumen es desechable, borralo y que se recree (los nuevos copian
  el dueño correcto de la imagen). Si tiene datos:
  `docker run --rm -v ovaultd-datos:/data alpine chown -R 10001:10001 /data`.

## E2E del empaquetado

```bash
bash tests/scripts/container-e2e.sh
```

Construye la imagen real, espera el healthcheck de Docker, corre el smoke completo contra el
server (HTTP + WebSocket, cripto real) más el sync E2E del runtime (Chromium) y verifica
persistencia con un recreate. Los artefactos quedan en `.tmp/container-e2e/`. La validación a
través del borde es un smoke manual contra NPM Plus (los tres puntos de arriba).

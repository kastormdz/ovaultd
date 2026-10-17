#!/usr/bin/env bash
# container-e2e.sh — E2E del empaquetado (fase 1):
#
#   1. build del overlay de dev (imagen local + nginx de ejemplo, el MISMO conf del repo)
#   2. healthcheck de Docker en `healthy` + /healthz directo y por proxy + /metrics
#   3. logs JSON del contenedor
#   4. el smoke COMPLETO a TRAVÉS del proxy reverso (HTTP + WebSocket, cripto real)
#   5. persistencia: recreate y la base sobrevive
#
# Uso:  bash tests/scripts/container-e2e.sh
# Artefactos: .tmp/container-e2e/ (bind mount del /data del contenedor). Deja todo APAGADO.

set -euo pipefail

RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$RAIZ"

DIR="$RAIZ/.tmp/container-e2e"
export OVAULTD_PORT="${E2E_OVAULTD_PORT:-8890}"
export OVAULTD_PROXY_PORT="${E2E_PROXY_PORT:-8888}"
export OVAULTD_ADDR="127.0.0.1"
export OVAULTD_DATA="$DIR/datos"
export E2E_UID="$(id -u)"
export E2E_GID="$(id -g)"

COMPOSE=(docker compose -f compose.yaml -f docker/compose.dev.yaml -f tests/scripts/container-e2e.compose.yaml)

paso() { printf '\n== %s\n' "$*"; }
nota() { printf '   %s\n' "$*"; }
fallo() {
    printf '\nFALLO: %s\n' "$*" >&2
    "${COMPOSE[@]}" logs --tail 40 ovaultd 2>/dev/null || true
    echo "--- proxy ---" >&2
    "${COMPOSE[@]}" logs --tail 20 proxy 2>/dev/null || true
    exit 1
}

esperar_healthy() {
    local cid estado
    cid="$("${COMPOSE[@]}" ps -q ovaultd)"
    [ -n "$cid" ] || return 1
    for _ in $(seq 1 60); do
        estado="$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$cid" 2>/dev/null || echo unknown)"
        if [ "$estado" = "healthy" ]; then
            echo "$cid"
            return 0
        fi
        sleep 1
    done
    return 1
}

# El nginx del overlay arranca DESPUÉS del healthy del server: hasta que bindea su
# puerto, el socket del host es un RST (sin listener). Se espera con paciencia, nunca
# con un curl seco — la carrera se pierde sola cuando el host está cargado (medido
# 2026-10-17: el curl del recreate llegó 7 s antes de que nginx hiciera bind).
esperar_proxy() {
    local respuesta=""
    for _ in $(seq 1 60); do
        respuesta="$(curl -fsS "http://127.0.0.1:$OVAULTD_PROXY_PORT/healthz" 2>/dev/null || true)"
        if [ -n "$respuesta" ]; then
            printf '%s' "$respuesta"
            return 0
        fi
        sleep 0.5
    done
    return 1
}

docker compose version >/dev/null 2>&1 || fallo "falta el plugin docker compose"

# ---------------------------------------------------------------- 1. build + up

paso "terreno"
"${COMPOSE[@]}" down --remove-orphans >/dev/null 2>&1 || true
rm -rf "$DIR"
mkdir -p "$DIR/datos"
nota "datos del contenedor en $OVAULTD_DATA (bind mount)"

paso "build (overlay de dev: imagen local + nginx)"
"${COMPOSE[@]}" build

paso "up"
"${COMPOSE[@]}" up -d

# ---------------------------------------------------------------- 2. salud

paso "salud (docker healthcheck + /healthz directo y por proxy)"
CIDS="$(esperar_healthy)" || fallo "el contenedor no llegó a healthy en 60 s"
nota "docker: healthy ($CIDS)"

DIRECTO="$(curl -fsS "http://127.0.0.1:$OVAULTD_PORT/healthz")" \
    || fallo "el server directo no responde en $OVAULTD_PORT"
nota "directo ($OVAULTD_PORT): $DIRECTO"

# El nginx del overlay arranca DESPUÉS del healthy del server: hasta que bindea el :80, el
# socket del host es un RST (sin listener). Se espera con paciencia, no con un curl seco.
PROXY="$(esperar_proxy)" || fallo "el proxy no responde en $OVAULTD_PROXY_PORT"
nota "por proxy ($OVAULTD_PROXY_PORT): $PROXY"

paso "/metrics: directo y por proxy"
curl -fsS "http://127.0.0.1:$OVAULTD_PORT/metrics" | grep -q "ovaultd_up 1" \
    || fallo "/metrics directo no expone ovaultd_up"
nota "directo: ovaultd_up 1 + uptime"
curl -fsS "http://127.0.0.1:$OVAULTD_PROXY_PORT/metrics" | grep -q "ovaultd_up 1" \
    || fallo "/metrics por proxy no expone ovaultd_up"
nota "por proxy: ovaultd_up 1 (red privada habilitada en el conf)"

# ---------------------------------------------------------------- 3. logs json

paso "logs JSON del contenedor"
# `sed -n 1p` y no `head -1`: head cierra el pipe temprano y con `pipefail` eso mata el script.
PRIMERA="$("${COMPOSE[@]}" logs --no-log-prefix --tail 20 ovaultd | sed -n '1p')"
if jq -e . >/dev/null 2>&1 <<<"$PRIMERA"; then
    nota "primera línea parsea como JSON"
else
    fallo "los logs no son JSON: $PRIMERA"
fi
jq -e 'has("fields") or has("message") or has("level")' >/dev/null 2>&1 <<<"$PRIMERA" \
    && nota "estructura esperada (level/target/fields)"

# ---------------------------------------------------------------- 4. smoke por proxy

paso "smoke COMPLETO a través del proxy (HTTP + WS, cripto real)"
SMOKE_BASE="http://127.0.0.1:$OVAULTD_PROXY_PORT" \
SMOKE_WS_BASE="ws://127.0.0.1:$OVAULTD_PROXY_PORT" \
SMOKE_DB="$DIR/datos/metadata.db" \
SMOKE_DIR=".tmp/container-e2e/smoke" \
    bash tests/scripts/server-smoke.sh

# ---------------------------------------------------------------- 5. persistencia

paso "persistencia: recreate"
"${COMPOSE[@]}" down >/dev/null 2>&1
"${COMPOSE[@]}" up -d >/dev/null
CIDS="$(esperar_healthy)" || fallo "no llegó a healthy tras el recreate"
VAULTS="$(python3 - "$DIR/datos/metadata.db" <<'PY'
import sqlite3, sys
con = sqlite3.connect(sys.argv[1])
print(con.execute("select count(*) from vaults").fetchone()[0])
PY
)"
[ "$VAULTS" -ge 1 ] || fallo "la base no persistió el vault del smoke"
nota "vaults en la base tras recreate: $VAULTS"
esperar_proxy >/dev/null || fallo "el proxy no responde tras el recreate"
nota "healthz por proxy tras recreate: ok"

# ---------------------------------------------------------------- cierre

paso "bajando todo"
"${COMPOSE[@]}" down >/dev/null 2>&1
nota "artefactos en $DIR"
printf '\nCONTAINER-E2E OK\n'

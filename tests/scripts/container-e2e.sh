#!/usr/bin/env bash
# container-e2e.sh — E2E del empaquetado (fase 1):
#
#   1. build del overlay de dev (imagen local; sin proxy: el borde es del operador, ADR-0019)
#   2. healthcheck de Docker en `healthy` + /healthz + /metrics directo contra el server
#   3. logs JSON del contenedor
#   4. el smoke COMPLETO contra el server (HTTP + WebSocket, cripto real)
#   4b. sync E2E: el runtime (Chromium) baja la delta del smoke y la aplica
#   5. persistencia: recreate y la base sobrevive
#
# Uso:  bash tests/scripts/container-e2e.sh
# Artefactos: .tmp/container-e2e/ (bind mount del /data del contenedor). Deja todo APAGADO.

set -euo pipefail

RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$RAIZ"

DIR="$RAIZ/.tmp/container-e2e"
export OVAULTD_PORT="${E2E_OVAULTD_PORT:-8890}"
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

# El bind del puerto del host puede llegar DESPUÉS del healthy del contenedor en hosts
# cargados: hasta que hay listener, el socket es un RST. Se espera con paciencia, nunca
# con un curl seco.
esperar_server() {
    local respuesta=""
    for _ in $(seq 1 60); do
        respuesta="$(curl -fsS "http://127.0.0.1:$OVAULTD_PORT/healthz" 2>/dev/null || true)"
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

paso "build (overlay de dev: imagen local)"
"${COMPOSE[@]}" build

paso "up"
"${COMPOSE[@]}" up -d

# ---------------------------------------------------------------- 2. salud

paso "salud (docker healthcheck + /healthz)"
CIDS="$(esperar_healthy)" || fallo "el contenedor no llegó a healthy en 60 s"
nota "docker: healthy ($CIDS)"

DIRECTO="$(esperar_server)" || fallo "el server no responde en $OVAULTD_PORT"
nota "http://127.0.0.1:$OVAULTD_PORT/healthz: $DIRECTO"

paso "/metrics"
curl -fsS "http://127.0.0.1:$OVAULTD_PORT/metrics" | grep -q "ovaultd_up 1" \
    || fallo "/metrics no expone ovaultd_up"
nota "ovaultd_up 1 + uptime"

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

# ---------------------------------------------------------------- 4. smoke directo

paso "smoke COMPLETO contra el server (HTTP + WS, cripto real)"
SMOKE_BASE="http://127.0.0.1:$OVAULTD_PORT" \
SMOKE_WS_BASE="ws://127.0.0.1:$OVAULTD_PORT" \
SMOKE_DB="$DIR/datos/metadata.db" \
SMOKE_DIR=".tmp/container-e2e/smoke" \
    bash tests/scripts/server-smoke.sh

# ---------------------------------------------------------------- 4b. sync E2E

paso "sync E2E: el runtime (Chromium) baja la delta del smoke y la aplica"
# El estado del vault y los plaintext que dejó el smoke-cliente son la fuente de verdad:
# el runtime real (Chromium + IndexedDB + puente /fs) hace signin, abre el WS con el
# token en subprotocolo (ADR-0015) y aplica la delta firmada por el cliente canónico en
# Rust, byte a byte. Correr DESPUÉS del smoke y ANTES del recreate.
( cd tests && OVAULTD_SYNC_E2E=1 \
    OVAULTD_SYNC_BASE="http://127.0.0.1:$OVAULTD_PORT" \
    OVAULTD_SYNC_STATE="$DIR/smoke/cliente/estado.json" \
    OVAULTD_SYNC_SMOKE="$DIR/smoke" \
    npx playwright test browser/sync-pull.spec.ts --reporter=line ) \
    || fallo "el E2E de sync falló"
nota "upgrade por subprotocolo + delta de 2 revisiones aplicada byte a byte"

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
esperar_server >/dev/null || fallo "el server no responde tras el recreate"
nota "healthz tras recreate: ok"

# ---------------------------------------------------------------- cierre

paso "bajando todo"
"${COMPOSE[@]}" down >/dev/null 2>&1
nota "artefactos en $DIR"
printf '\nCONTAINER-E2E OK\n'

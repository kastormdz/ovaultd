#!/usr/bin/env bash
# server-smoke.sh — el flujo REAL contra el binario: crea un vault, confirma dos
# revisiones y comprueba la delta exacta.
#
# curl para toda la superficie HTTP; `smoke-cliente` (example del crate) firma los
# cuerpos con cripto real y habla la sesion WS (hello -> auth -> batch), que es el
# unico camino de commit. Nada de firmas constantes ni credenciales de produccion:
# cada corrida genera claves nuevas y usa parametros de KDF reducidos a proposito.
#
# Uso:  bash tests/scripts/server-smoke.sh
# Artefactos: .tmp/smoke/ (base, CAS, log del server, estado del cliente).
# Los usa tests/scripts/scan-canary.sh para la canaria de plaintext.

set -euo pipefail

RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$RAIZ"

PUERTO="${SMOKE_PORT:-8791}"

# Modo externo: correr el flujo COMPLETO contra un server ya levantado (p. ej. el contenedor
# detrás de un proxy). SMOKE_BASE=http://host:puerto; SMOKE_DB tiene que apuntar a la MISMA base
# que sirve ese server (el init del cliente escribe la cuenta ahí); SMOKE_WS_BASE si el WebSocket
# no vive en el mismo host:puerto que el HTTP.
EXTERNO="${SMOKE_BASE:-}"
BASE="${SMOKE_BASE:-http://127.0.0.1:$PUERTO}"
WS_BASE="${SMOKE_WS_BASE:-ws://127.0.0.1:$PUERTO}"
DIR="${SMOKE_DIR:-.tmp/smoke}"
CLI="$DIR/cliente"
REGISTRO="$DIR/server.log"
TARGET="${CARGO_TARGET_DIR:-$RAIZ/target}"
DB_CLIENTE="${SMOKE_DB:-$PWD/$DIR/db/metadata.db}"
PID_SERVIDOR=""

paso() { printf '\n== %s\n' "$*"; }
nota() { printf '   %s\n' "$*"; }
fallo() {
    printf '\nFALLO: %s\n' "$*" >&2
    if [ -f "$REGISTRO" ]; then
        printf '\n--- ultimas lineas del server ---\n' >&2
        tail -20 "$REGISTRO" >&2
    fi
    exit 1
}

# ---------------------------------------------------------------- 0. build

if [ -n "$EXTERNO" ]; then
    paso "modo externo contra $BASE (sin build del server)"
else
    paso "build (ovaultd + smoke-cliente)"
fi
cargo build -p ovaultd-server --example smoke-cliente
CLIENTE="$TARGET/debug/examples/smoke-cliente"
[ -x "$CLIENTE" ] || fallo "no existe el cliente de prueba $CLIENTE"
if [ -z "$EXTERNO" ]; then
    cargo build -p ovaultd-server --bin ovaultd
    SERVIDOR="$TARGET/debug/ovaultd"
    [ -x "$SERVIDOR" ] || fallo "no existe el binario $SERVIDOR"
fi

# ---------------------------------------------------------------- 1. terreno

paso "terreno fresco en $DIR"
rm -rf "$DIR"
mkdir -p "$CLI"
if [ -n "$EXTERNO" ]; then
    [ -d "$(dirname "$DB_CLIENTE")" ] || fallo "el directorio de SMOKE_DB no existe: $(dirname "$DB_CLIENTE")"
    nota "base compartida con el server externo: $DB_CLIENTE"
else
    mkdir -p "$DIR/db" "$DIR/cas"
fi

CANARIA="CANARIO-SMOKE-$(head -c 8 /dev/urandom | od -An -tx1 | tr -d ' \n')"
EMAIL="smoke-$(date +%s)@example.test"
PASSWORD="password-de-prueba-del-smoke"

printf '# Nota uno\ncanaria: %s\n' "$CANARIA" > "$CLI/nota-1.txt"
printf '# Nota dos\ncanaria: %s\n' "$CANARIA" > "$CLI/nota-2.txt"
printf '%s\n' "$CANARIA" > "$DIR/canaria.txt"
printf '%s\n' "$PASSWORD" > "$DIR/password.txt"

# ---------------------------------------------------------------- 2. server

if [ -n "$EXTERNO" ]; then
    paso "server externo: $BASE (esperando)"
else
    paso "ovaultd en $BASE (log: $REGISTRO)"
    OVAULTD_BIND="127.0.0.1:$PUERTO" \
    OVAULTD_DB="$DB_CLIENTE" \
    OVAULTD_CAS="$PWD/$DIR/cas" \
        "$SERVIDOR" > "$REGISTRO" 2>&1 &
    PID_SERVIDOR=$!
    trap 'kill "$PID_SERVIDOR" 2>/dev/null || true; wait "$PID_SERVIDOR" 2>/dev/null || true' EXIT
fi

CODIGO="000"
for _ in $(seq 1 100); do
    CODIGO="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/api/v1/me" || true)"
    [ "$CODIGO" = "401" ] && break
    if [ -n "$PID_SERVIDOR" ]; then
        kill -0 "$PID_SERVIDOR" 2>/dev/null || fallo "el server murio al arrancar"
    fi
    sleep 0.1
done
[ "$CODIGO" = "401" ] || fallo "el server no responde en $BASE (codigo $CODIGO)"
nota "arriba (401 sin token, como corresponde)"

# ---------------------------------------------------------------- 3. cuenta

paso "cuenta + material del vault (init)"
"$CLIENTE" init --state "$CLI/estado.json" --db "$DB_CLIENTE" \
    --email "$EMAIL" --password "$PASSWORD"

# ---------------------------------------------------------------- 4. signin

paso "signin (curl)"
CUERPO_SIGNIN="$(jq -nc --arg email "$EMAIL" --arg password "$PASSWORD" \
    '{email: $email, password: $password, name: "smoke", platform: "ci"}')"
SIGNIN="$(curl -sf -X POST "$BASE/api/v1/auth/signin" \
    -H 'content-type: application/json' -d "$CUERPO_SIGNIN")" \
    || fallo "signin no respondio 2xx"
TOKEN="$(jq -r .token <<<"$SIGNIN")"
DEVICE="$(jq -r .device_id <<<"$SIGNIN")"
[ -n "$TOKEN" ] && [ "$TOKEN" != "null" ] || fallo "signin sin token: $SIGNIN"
AUTH=(-H "authorization: Bearer $TOKEN")
nota "device=$DEVICE token=${TOKEN:0:8}… (recortado)"

# ---------------------------------------------------------------- 5. vault

paso "crear vault (curl + material firmado)"
CUERPO_VAULT="$("$CLIENTE" material --state "$CLI/estado.json")"
VAULT="$(curl -sf -X POST "$BASE/api/v1/vaults" "${AUTH[@]}" \
    -H 'content-type: application/json' -d "$CUERPO_VAULT" | jq -r .id)" \
    || fallo "POST /vaults no respondio 2xx"
[ -n "$VAULT" ] && [ "$VAULT" != "null" ] || fallo "el vault no tiene id"
nota "vault=$VAULT"

# ---------------------------------------------------------------- 6. access

paso "challenge + access (proof firmado)"
CHALLENGE="$(curl -sf "$BASE/api/v1/vaults/$VAULT/challenge" "${AUTH[@]}" | jq -r .challenge)" \
    || fallo "GET /challenge no respondio 2xx"
CUERPO_PROOF="$("$CLIENTE" proof --state "$CLI/estado.json" \
    --vault "$VAULT" --device "$DEVICE" --challenge "$CHALLENGE")"
ACCESS="$(curl -sf -X POST "$BASE/api/v1/vaults/$VAULT/access" "${AUTH[@]}" \
    -H 'content-type: application/json' -d "$CUERPO_PROOF")" \
    || fallo "POST /access rechazo el proof"
nota "access: $(jq -c . <<<"$ACCESS")"

# ---------------------------------------------------------------- 7. commits

# $1 = nota, $2 = path, $3 = expected_rev -> confirma la revision y guarda el ack
commit_rev() {
    local nota="$1" path="$2" base="$3"
    local nombre
    nombre="$(basename "$nota" .txt)"

    "$CLIENTE" encrypt --state "$CLI/estado.json" --content-file "$nota" \
        --out "$CLI/$nombre.bin" > "$CLI/$nombre.blob_id"
    local hash
    hash="$(cat "$CLI/$nombre.blob_id")"

    local codigo
    codigo="$(curl -s -o /dev/null -w '%{http_code}' -X PUT \
        "$BASE/api/v1/vaults/$VAULT/blobs/$hash" "${AUTH[@]}" \
        -H 'content-type: application/octet-stream' \
        --data-binary "@$CLI/$nombre.bin")"
    [ "$codigo" = "201" ] || fallo "PUT del blob $nombre dio $codigo (se esperaba 201)"

    "$CLIENTE" commit --state "$CLI/estado.json" --ws-url "$WS_BASE/api/v1/ws" \
        --vault "$VAULT" --device "$DEVICE" --token "$TOKEN" \
        --path "$path" --blob-file "$CLI/$nombre.bin" --expected-rev "$base" \
        > "$CLI/ack-$base.json" \
        || fallo "commit de $nombre (base $base) fallo"

    jq -e --argjson base "$base" '.t == "batch_ack" and .rev == ($base + 1)' \
        "$CLI/ack-$base.json" > /dev/null \
        || fallo "ack inesperado para $nombre: $(cat "$CLI/ack-$base.json")"
    nota "rev $((base + 1)): $path (blob $(printf '%s' "$hash" | cut -c1-12)…)"
}

paso "revision 1: add Notas/uno.md"
commit_rev "$CLI/nota-1.txt" "Notas/uno.md" 0

paso "revision 2: add Notas/dos.md"
commit_rev "$CLI/nota-2.txt" "Notas/dos.md" 1

# ---------------------------------------------------------------- 8. delta

paso "delta exacta"
DELTA="$(curl -sf "$BASE/api/v1/vaults/$VAULT/changes?since=0&limit=100" "${AUTH[@]}")" \
    || fallo "GET /changes no respondio 2xx"

jq -e '.next_since == 2 and .min_rev == null and (.items | length) == 2' \
    <<<"$DELTA" > /dev/null || fallo "delta inesperada: $DELTA"
jq -e '[.items[].rev] == [1, 2] and [.items[].op] == ["add", "add"]' \
    <<<"$DELTA" > /dev/null || fallo "revs/ops inesperados: $DELTA"

ACK1="$(cat "$CLI/ack-0.json")"
ACK2="$(cat "$CLI/ack-1.json")"
jq -e --argjson a1 "$ACK1" --argjson a2 "$ACK2" \
    '.items[0].path_id == $a1.path_id and .items[0].blob_id == $a1.blob_id
     and .items[1].path_id == $a2.path_id and .items[1].blob_id == $a2.blob_id' \
    <<<"$DELTA" > /dev/null \
    || fallo "los ids de la delta no coinciden con lo que firmo el cliente: $DELTA"

DELTA_1="$(curl -sf "$BASE/api/v1/vaults/$VAULT/changes?since=1&limit=100" "${AUTH[@]}")" \
    || fallo "GET /changes?since=1 no respondio 2xx"
jq -e '.next_since == 2 and [.items[].rev] == [2]' <<<"$DELTA_1" > /dev/null \
    || fallo "delta desde 1 inesperada: $DELTA_1"

nota "2 revisiones, ids coincidentes, since=1 devuelve solo la rev 2"

# ---------------------------------------------------------------- 9. cierre

paso "resumen"
nota "canaria del smoke: $CANARIA"
if [ -n "$EXTERNO" ]; then
    nota "artefactos en $DIR (server externo: este script no lo apaga)"
else
    nota "artefactos en $DIR (el server se apaga solo)"
fi
printf '\nSMOKE OK\n'

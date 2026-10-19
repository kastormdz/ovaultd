#!/usr/bin/env bash
# cli-e2e.sh — E2E de la CLI del núcleo (fase 2): server real + cuenta real + flujo completo.
#
#   /setup (bootstrap de la primera cuenta por web) → login → create-remote → setup →
#   list-local → sync (dos pasadas: add; mod/del/add) → list-local (todo igual, outbox
#   vacía) → history/preview/restore (lectura pura; el restore sube como revisión NUEVA)
#   → config → unlink --purge
#
# Verifica contra la base del server: las revisiones quedaron con los ops correctos
# (add,add / add,mod,del). Nada de mocks: el push real firma msg_rec y commitea por WS.
#
# Uso:  bash tests/scripts/cli-e2e.sh          (requiere node + build TS del CLI)
# Artefactos: .tmp/cli-e2e/ (gitignored). El server se apaga solo (trap).

set -euo pipefail

RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$RAIZ"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/cargo-target/ovaultd}"

D="${CLI_E2E_DIR:-$RAIZ/.tmp/cli-e2e}"
PUERTO="${CLI_E2E_PORT:-8891}"
BIN="$CARGO_TARGET_DIR/debug/ovaultd"
ADMIN="$CARGO_TARGET_DIR/debug/ovaultd-admin"
CLI=(node "$RAIZ/packages/cli/dist/main.js")

paso() { printf '\n== %s\n' "$*"; }
nota() { printf '   %s\n' "$*"; }
fallo() {
    printf '\nFALLO: %s\n' "$*" >&2
    [ -f "$D/server.log" ] && { printf '\n--- server ---\n' >&2; tail -10 "$D/server.log" >&2; }
    exit 1
}

[ -f "$RAIZ/packages/cli/dist/main.js" ] || fallo "falta el build del CLI: npm run build"

paso "build (binarios debug)"
cargo build -p ovaultd-server --bin ovaultd --bin ovaultd-admin 2>&1 | tail -1

paso "terreno fresco en $D"
rm -rf "$D"
mkdir -p "$D/datos" "$D/cas" "$D/config" "$D/runtime" "$D/vault/Notas"

paso "server en :$PUERTO"
OVAULTD_BIND="127.0.0.1:$PUERTO" OVAULTD_DB="$D/datos/metadata.db" OVAULTD_CAS="$D/cas" \
    "$BIN" > "$D/server.log" 2>&1 &
PID=$!
trap 'kill "$PID" 2>/dev/null || true' EXIT

CODIGO="000"
for _ in $(seq 1 100); do
    CODIGO="$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PUERTO/api/v1/me" || true)"
    [ "$CODIGO" = "401" ] && break
    sleep 0.1
done
[ "$CODIGO" = "401" ] || fallo "el server no respondió en :$PUERTO"
nota "arriba (401 sin token)"

paso "bootstrap por web: /setup crea la primera cuenta"
FORMULARIO="$(curl -sf "http://127.0.0.1:$PUERTO/setup")"
grep -q 'name="email"' <<<"$FORMULARIO" || fallo "el formulario de /setup no aparece"
CUENTA_EMAIL='cli-e2e@example.test'
CUENTA_PW='password-de-cuenta-cli'
curl -sf -X POST "http://127.0.0.1:$PUERTO/setup" \
    --data-urlencode "email=$CUENTA_EMAIL" --data-urlencode "password=$CUENTA_PW" > /dev/null
CODIGO_SETUP="$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PUERTO/setup" \
    --data-urlencode "email=otra@example.test" --data-urlencode "password=password-de-cuenta-otra")"
[ "$CODIGO_SETUP" = "410" ] || fallo "/setup no se cerró tras la primera cuenta (dio $CODIGO_SETUP)"

paso "cuentas siguientes con ovaultd-admin (la segunda la ve inspect)"
printf 'password-de-cuenta-dos\n' | "$ADMIN" --db "$D/datos/metadata.db" create-account --email segunda@example.test > /dev/null
"$ADMIN" --db "$D/datos/metadata.db" inspect | grep -Eq 'cuentas: +2' || fallo "inspect no ve las 2 cuentas"

export OVAULTD_CONFIG_DIR="$D/config"
export OVAULTD_RUNTIME_DIR="$D/runtime"

paso "login (password de CUENTA por stdin)"
echo 'password-de-cuenta-cli' | "${CLI[@]}" login "http://127.0.0.1:$PUERTO" --email cli-e2e@example.test --password-stdin

paso "create-remote (password DEL VAULT, independiente — ADR-0011)"
echo 'password-vault-cli' | "${CLI[@]}" create-remote notas --password-stdin --recovery-out "$D/recovery.txt"

paso "setup"
echo 'password-vault-cli' | "${CLI[@]}" setup 1 --dir "$D/vault" --password-stdin

paso "dos archivos locales + list-local (2 nuevos)"
printf '# Nota uno\nhola desde la CLI\n' > "$D/vault/Notas/uno.md"
printf '# Nota dos\n' > "$D/vault/Notas/dos.md"
echo 'password-vault-cli' | "${CLI[@]}" list-local 1 --password-stdin

paso "sync 1 (sube 2)"
echo 'password-vault-cli' | "${CLI[@]}" sync 1 --password-stdin

TOKEN="$(jq -r .token "$D/config/session.json")"
DELTA="$(curl -sf "http://127.0.0.1:$PUERTO/api/v1/vaults/1/changes?since=0&limit=100" -H "authorization: Bearer $TOKEN")"
nota "delta: $(jq -c '[.items[].op]' <<<"$DELTA")"
jq -e '(.items | length) == 2 and [.items[].op] == ["add","add"]' <<<"$DELTA" > /dev/null \
    || fallo "delta inesperada tras el primer sync"

paso "sync 2 (mod + del + add → rev 3..5)"
printf '# Nota uno\nEditada por la CLI\n' > "$D/vault/Notas/uno.md"
rm "$D/vault/Notas/dos.md"
printf 'nueva\n' > "$D/vault/tres.md"
echo 'password-vault-cli' | "${CLI[@]}" sync 1 --password-stdin

DELTA2="$(curl -sf "http://127.0.0.1:$PUERTO/api/v1/vaults/1/changes?since=2&limit=100" -H "authorization: Bearer $TOKEN")"
nota "delta nueva: $(jq -c '[.items[].op]' <<<"$DELTA2")"
jq -e '(.items | length) == 3' <<<"$DELTA2" > /dev/null || fallo "delta inesperada tras el segundo sync"
jq -e '[.items[].op] | sort == ["add","del","mod"]' <<<"$DELTA2" > /dev/null \
    || fallo "ops inesperados (¿firma del del?)"

paso "list-local (todo en 'igual', outbox vacía)"
SALIDA="$(echo 'password-vault-cli' | "${CLI[@]}" list-local 1 --password-stdin)"
echo "$SALIDA" | head -4
echo "$SALIDA" | grep -q "sinCambios=2" || fallo "list-local no ve todo sincronizado"

paso "history (lectura pura: revs 2 y 4 de Notas/uno.md)"
# Las revisiones del vault son POR PATH y el push va 1 op por lote: dos.md fue la rev 1,
# uno.md nació en la 2 y su edición quedó en la 4 (la historia de un archivo muestra solo las suyas).
HIST="$(echo 'password-vault-cli' | "${CLI[@]}" history Notas/uno.md --password-stdin)"
echo "$HIST" | head -4
echo "$HIST" | grep -qE '^rev +4 +mod ' || fallo "history no lista la rev 4 (mod)"
echo "$HIST" | grep -qE '^rev +2 +add ' || fallo "history no lista la rev 2 (add)"

paso "preview de la rev 2: el contenido viejo, sin tocar el vault"
MD5_ANTES="$(md5sum "$D/vault/Notas/uno.md" | awk '{print $1}')"
PREV="$(echo 'password-vault-cli' | "${CLI[@]}" preview Notas/uno.md --rev 2 --password-stdin)"
echo "$PREV" | head -3
echo "$PREV" | grep -q 'Nota uno' || fallo "preview no muestra el contenido de la rev 2"
if echo "$PREV" | grep -q 'Editada por la CLI'; then fallo "preview mostró el contenido NUEVO para la rev 2"; fi
grep -q 'Editada por la CLI' "$D/vault/Notas/uno.md" || fallo "preview MUTÓ el archivo del vault"
if [ "$(md5sum "$D/vault/Notas/uno.md" | awk '{print $1}')" != "$MD5_ANTES" ]; then fallo "preview cambió el archivo (md5)"; fi

paso "restore sin --yes: solo muestra qué cambiaría (no aplica)"
DRY="$(echo 'password-vault-cli' | "${CLI[@]}" restore Notas/uno.md --rev 2 --password-stdin)"
echo "$DRY" | head -4
if [ "$(md5sum "$D/vault/Notas/uno.md" | awk '{print $1}')" != "$MD5_ANTES" ]; then fallo "restore sin --yes tocó el archivo"; fi

paso "restore --yes de la rev 2 + sync: la vuelta al pasado sube como revisión NUEVA (rev 6)"
echo 'password-vault-cli' | "${CLI[@]}" restore Notas/uno.md --rev 2 --yes --password-stdin
grep -q 'Nota uno' "$D/vault/Notas/uno.md" || fallo "el archivo no quedó con el contenido de la rev 1"
if grep -q 'Editada por la CLI' "$D/vault/Notas/uno.md"; then fallo "el restore no reemplazó la edición"; fi
echo 'password-vault-cli' | "${CLI[@]}" sync 1 --password-stdin

DELTA3="$(curl -sf "http://127.0.0.1:$PUERTO/api/v1/vaults/1/changes?since=5&limit=100" -H "authorization: Bearer $TOKEN")"
nota "delta del restore: $(jq -c '[.items[] | {rev, op}]' <<<"$DELTA3")"
jq -e '(.items | length) == 1 and .items[0].op == "mod" and .items[0].rev == 6' <<<"$DELTA3" > /dev/null \
    || fallo "el restore no quedó como revisión NUEVA (rev 6 mod)"

PREV6="$(echo 'password-vault-cli' | "${CLI[@]}" preview Notas/uno.md --rev 6 --password-stdin)"
echo "$PREV6" | grep -q 'Nota uno' || fallo "la rev 6 no tiene el contenido restaurado"

paso "config"
"${CLI[@]}" config | head -8

paso "unlink --purge"
"${CLI[@]}" unlink 1 --purge

printf '\nCLI-E2E OK\n'

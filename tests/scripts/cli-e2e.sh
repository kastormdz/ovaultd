#!/usr/bin/env bash
# cli-e2e.sh — E2E de la CLI del núcleo (fase 2): server real + cuenta real + flujo completo.
#
#   login → create-remote → setup → list-local → sync (dos pasadas: add; mod/del/add)
#   → list-local (todo igual, outbox vacía) → config → unlink --purge
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

paso "cuenta con ovaultd-admin"
printf 'password-de-cuenta-cli\n' | "$ADMIN" --db "$D/datos/metadata.db" create-account --email cli-e2e@example.test

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

paso "config"
"${CLI[@]}" config | head -8

paso "unlink --purge"
"${CLI[@]}" unlink 1 --purge

printf '\nCLI-E2E OK\n'

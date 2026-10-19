#!/usr/bin/env bash
# e2e-conflicto.sh — prueba end-to-end de la política de conflictos contra un server real.
#
# Dos réplicas locales (A y B) del mismo vault editan la MISMA línea sin verse. El pull de
# A debe CONSERVAR AMBOS (nota.md = versión del server, `<nombre> (conflicto ...).<ext>` =
# edición local), subir la copia y dejar los dos ciclos convergentes — nunca bloqueados.
# Al final, B baja la copia: el contenido local de A también llegó al server.
#
# Requiere un server arriba y una sesión YA guardada (la del bench sirve: no hay login):
#
#   E2E_PW        password del vault de prueba (se inventa en cada corrida; NO va al repo)
#   E2E_BASE      server (default http://127.0.0.1:8894)
#   E2E_SESSION   dir con session.json de una cuenta (default .tmp/bench/config)
#   E2E_KEEP=1    no borra el vault remoto de prueba al terminar
set -euo pipefail

RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$RAIZ"

BASE="${E2E_BASE:-http://127.0.0.1:8894}"
SESION="${E2E_SESSION:-$RAIZ/.tmp/bench/config}"
: "${E2E_PW:?falta E2E_PW (password del vault de prueba)}"
[ -f "$SESION/session.json" ] || { echo "FALLO: no hay sesión en $SESION (corré antes el bench o pasá E2E_SESSION)" >&2; exit 1; }

CLI=(node "$RAIZ/packages/cli/dist/main.js")
T="$(mktemp -d "${TMPDIR:-/tmp}/e2e-conflicto.XXXX")"
export OVAULTD_PASSWORD="$E2E_PW"

paso() { printf '\n== %s\n' "$*"; }
fallo() { printf '\nFALLO: %s\n' "$*" >&2; exit 1; }

paso "0. configs A y B (misma sesión, runtimes aislados)"
mkdir -p "$T/A/config" "$T/B/config"
cp -r "$SESION/." "$T/A/config/"
cp -r "$SESION/." "$T/B/config/"

paso "1. vault remoto nuevo"
printf '%s' "$E2E_PW" | OVAULTD_CONFIG_DIR="$T/A/config" "${CLI[@]}" create-remote conflicto-e2e \
    --password-stdin --recovery-out "$T/recovery.txt" >/dev/null
VFILE="$(ls "$T/A/config/vaults/"*_*_*.json | tail -1)"
VID="$(basename "$VFILE" .json | sed 's/.*_//')"
echo "vault id: $VID"

paso "2. vault A: nota base + setup + sync"
mkdir -p "$T/vaultA"
printf 'El gato duerme en el sofa.\nlinea dos\n' > "$T/vaultA/nota.md"
printf '%s' "$E2E_PW" | OVAULTD_CONFIG_DIR="$T/A/config" OVAULTD_RUNTIME_DIR="$T/A/runtime" \
    "${CLI[@]}" setup "$VID" --dir "$T/vaultA" --password-stdin >/dev/null
OVAULTD_CONFIG_DIR="$T/A/config" OVAULTD_RUNTIME_DIR="$T/A/runtime" "${CLI[@]}" sync "$VID" 2>&1 | tail -2

paso "3. vault B: setup + sync (baja la base)"
mkdir -p "$T/vaultB"
printf '%s' "$E2E_PW" | OVAULTD_CONFIG_DIR="$T/B/config" OVAULTD_RUNTIME_DIR="$T/B/runtime" \
    "${CLI[@]}" setup "$VID" --dir "$T/vaultB" --password-stdin >/dev/null
OVAULTD_CONFIG_DIR="$T/B/config" OVAULTD_RUNTIME_DIR="$T/B/runtime" "${CLI[@]}" sync "$VID" 2>&1 | tail -2
grep -q 'El gato duerme' "$T/vaultB/nota.md" || fallo "B no bajó la base"

paso "4. B edita la MISMA línea y sube"
printf 'El gato CORRE en el sofa.\nlinea dos\n' > "$T/vaultB/nota.md"
OVAULTD_CONFIG_DIR="$T/B/config" OVAULTD_RUNTIME_DIR="$T/B/runtime" "${CLI[@]}" sync "$VID" 2>&1 | tail -2

paso "5. A edita la misma línea (sin bajar la de B) y sincroniza"
printf 'El gato RONCA en el sofa.\nlinea dos\n' > "$T/vaultA/nota.md"
OVAULTD_CONFIG_DIR="$T/A/config" OVAULTD_RUNTIME_DIR="$T/A/runtime" "${CLI[@]}" sync "$VID" 2>&1 | tail -4

paso "6. A conserva los DOS contenidos"
grep -q 'El gato CORRE' "$T/vaultA/nota.md" || fallo "nota.md no quedó con la versión del server"
COPIA="$(ls "$T/vaultA" | grep -i conflicto | head -1 || true)"
[ -n "$COPIA" ] || fallo "no se creó la copia de conflicto"
grep -q 'El gato RONCA' "$T/vaultA/$COPIA" || fallo "la copia no tiene la edición local"
echo "nota.md      -> $(head -1 "$T/vaultA/nota.md")"
echo "$COPIA -> $(head -1 "$T/vaultA/$COPIA")"

paso "7. segundo sync de A: converge, no bloquea"
SALIDA="$(OVAULTD_CONFIG_DIR="$T/A/config" OVAULTD_RUNTIME_DIR="$T/A/runtime" "${CLI[@]}" sync "$VID" 2>&1 | tail -2)"
echo "$SALIDA"
echo "$SALIDA" | grep -qi 'conflicto' && fallo "el segundo ciclo reportó conflicto"

paso "8. B baja la copia: el contenido local de A también está en el server"
OVAULTD_CONFIG_DIR="$T/B/config" OVAULTD_RUNTIME_DIR="$T/B/runtime" "${CLI[@]}" sync "$VID" 2>&1 | tail -2
COPIA_B="$(ls "$T/vaultB" | grep -i conflicto | head -1 || true)"
[ -n "$COPIA_B" ] || fallo "B no recibió la copia de conflicto (no llegó al server)"
grep -q 'El gato RONCA' "$T/vaultB/$COPIA_B" || fallo "la copia en B no tiene la edición local de A"
echo "$COPIA_B -> $(head -1 "$T/vaultB/$COPIA_B")"

echo
echo "OK: conflicto conservado en ambos lados, ciclos convergentes."
echo "TMP=$T"

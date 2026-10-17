#!/usr/bin/env bash
# scan-canary.sh — la canaria de plaintext: nada de lo que el cliente cifro puede
# aparecer EN CLARO en el lado del servidor (SQLite, WAL, CAS, temporales, logs).
#
# Corre server-smoke.sh si no hay artefactos (el smoke cifra contenido con canaria
# embebida y sube solo el ciphertext), y despues busca la canaria del lado servidor.
#
# Uso:  bash tests/scripts/scan-canary.sh

set -euo pipefail

RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$RAIZ"

DIR=".tmp/smoke"
CANARIA_ARCHIVO="$DIR/canaria.txt"
PASSWORD_ARCHIVO="$DIR/password.txt"

fallo() {
    printf '\nFALLO: %s\n' "$*" >&2
    exit 1
}

if [ ! -f "$CANARIA_ARCHIVO" ] || [ ! -f "$DIR/db/metadata.db" ]; then
    printf '== no hay artefactos del smoke; corro server-smoke.sh primero\n'
    bash "$RAIZ/tests/scripts/server-smoke.sh"
fi

CANARIA="$(cat "$CANARIA_ARCHIVO")"
PASSWORD="$(cat "$PASSWORD_ARCHIVO")"

# 0. El fixture tambien es el test: si la canaria no esta en el plaintext del
#    cliente, el scan de abajo estaria midiendo nada.
grep -q -- "$CANARIA" "$DIR/cliente/nota-1.txt" \
    || fallo "la canaria no esta en el plaintext del cliente (fixture roto)"
grep -q -- "$CANARIA" "$DIR/cliente/nota-2.txt" \
    || fallo "la canaria no esta en el plaintext del cliente (fixture roto)"

# 1. Ausencia en TODO el lado servidor: base SQLite (+ WAL/shm), CAS, temporales y log.
#    Solo el ciphertext puede estar ahi; la canaria en claro, jamas.
ENCONTRADOS="$(grep -R -a -l -e "$CANARIA" \
    "$DIR/db" "$DIR/cas" "$DIR/server.log" 2>/dev/null || true)"

if [ -n "$ENCONTRADOS" ]; then
    printf 'archivos con la canaria en claro:\n%s\n' "$ENCONTRADOS" >&2
    fallo "la canaria aparecio EN CLARO del lado del servidor"
fi

# 2. La password de la cuenta tampoco: en la base vive el hash Argon2, nunca el texto.
if grep -R -a -q -e "$PASSWORD" "$DIR/db" "$DIR/server.log" 2>/dev/null; then
    fallo "la password en claro aparecio en el lado del servidor"
fi

printf 'canaria ausente en db/wal/cas/temporales/logs: OK\n'
printf 'password en claro ausente (solo el hash Argon2): OK\n'
printf '\nSCAN-CANARY OK\n'

#!/usr/bin/env bash
# scan-leaks.sh — gate de fuga para un repo público: credenciales, claves, rutas de
# home, rangos de red privados y (opcionalmente) TU topología personal.
#
# Este repo está pensado para publicarse: credenciales, claves privadas, paths de
# máquinas concretas e IPs internas NO pueden quedar ni en el árbol de trabajo ni en
# la historia. Correr antes de cada push y en la preparación de publicación; el hook
# de pre-commit (.githooks/pre-commit, activado con `git config core.hooksPath
# .githooks`) lo corre en modo --worktree.
#
# Uso:
#   scripts/scan-leaks.sh              # árbol + historia completa
#   scripts/scan-leaks.sh --worktree   # solo el árbol (rápido; es lo que corre el hook)
#
# Patrones personales: hostnames, usuarios, dominios e IPs de tu infraestructura NO
# van hardcodeados acá (publicar el escáner publicaría tu topología). Van en
# `scripts/leaks.local` (no versionado, ver `leaks.local.example`), que este script
# carga si existe. En un clon público simplemente no existe y corren los genéricos.
#
# Salida: una línea por hallazgo; exit 1 si hay alguno. Solo lectura.

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-full}"
cd "$REPO" || exit 2

# ---------------------------------------------------------------------------
# Patrones GENÉRICOS (publicables; no nombran a nadie).

# Credenciales literales. Los falsos de los tests y el vector NFKC (público por
# diseño) están vetados abajo; cualquier OTRO match se revisa a mano.
# El valor no puede tener espacios: un valor con espacios es una frase de UI
# («Cambiar password»), no un secreto. Las etiquetas sueltas del glosario se
# vetan por nombre, igual que los falsos de los tests.
SECRETS='(password|passwd|secret|token|api[_-]?key)["'"'"']?[[:space:]]*[:=][[:space:]]*["'"'"'][^"'"'"'[:space:]]{8,}'
SECRETS_VETADOS='password-de-prueba|token-de-prueba|contrase|\{PASSWORD\}|reset\?token=|"Password"|"Password de la cuenta"|"Contraseña"'
# `X="$(cmd)"` es una sustitución de shell, no un secreto literal.
# `X="${VAR}"` / `X: "${VAR:-default}"` es una expansión de variable, tampoco.
SECRETS_EXPANSION='[:=][[:space:]]*"\$\{'
SECRETS_SUSTITUCION='=[[:space:]]*["'"'"']\$[(]'
KEYS='BEGIN (RSA|OPENSSH|EC|DSA) PRIVATE KEY'

# Rutas de home de cualquier usuario: /home/<algo>/ no tiene por qué estar en un repo público.
HOME_PATHS='/home/[A-Za-z0-9._-]+/'

# Rangos de red privados (RFC1918) + CGNAT/Tailscale (100.64/10). Los CIDR de ejemplo
# del nginx (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16) no identifican a nadie y se
# exceptúan por línea.
REDES_PRIVADAS='(\b10\.[0-9]+\.[0-9]+\.[0-9]+|\b192\.168\.[0-9]+\.[0-9]+|\b172\.(1[6-9]|2[0-9]|3[01])\.[0-9]+\.[0-9]+|\b100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.[0-9]+\.[0-9]+)'

# Emails fuera de dominios de ejemplo/documentación.
EMAILS='[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}'
EMAILS_PERMITIDOS='example\.(com|test|org)|ejemplo\.com'

# ---------------------------------------------------------------------------
# Patrones PERSONALES (opcionales, no versionados): una regex por línea; `#` comenta.
local_file="$REPO/scripts/leaks.local"
PATTERNS_PERSONAL=""
if [ -f "$local_file" ]; then
  PATTERNS_PERSONAL="$(grep -v '^[[:space:]]*#' "$local_file" | grep -v '^[[:space:]]*$' | paste -sd'|' -)"
fi

hits=0

reportar() { # $1 = bloque de hallazgos ya formateado
  [ -n "$1" ] || return 0
  printf '%s\n' "$1" | cut -c1-200
  hits=$((hits + 1))
}

scan_archivo() { # $1 = archivo trackeado; imprime hallazgos con prefijo archivo:línea
  # Case-insensitive a propósito: los nombres propios capitalizados filtran igual.
  local f="$1" out=""
  out+="$(grep -IniE -- "$SECRETS" "$f" 2>/dev/null | grep -viE -- "$SECRETS_VETADOS" | grep -vE -- "$SECRETS_SUSTITUCION" | grep -vE -- "$SECRETS_EXPANSION")"$'\n'
  out+="$(grep -IniE -- "$KEYS" "$f" 2>/dev/null)"$'\n'
  out+="$(grep -InE -- "$HOME_PATHS" "$f" 2>/dev/null)"$'\n'
  out+="$(grep -InE -- "$REDES_PRIVADAS" "$f" 2>/dev/null | grep -vF -e '10.0.0.0/8' -e '172.16.0.0/12' -e '192.168.0.0/16')"$'\n'
  out+="$(grep -InoE -- "$EMAILS" "$f" 2>/dev/null | grep -viE -- "$EMAILS_PERMITIDOS")"$'\n'
  if [ -n "$PATTERNS_PERSONAL" ]; then
    out+="$(grep -IniE -- "$PATTERNS_PERSONAL" "$f" 2>/dev/null)"$'\n'
  fi
  out="$(printf '%s' "$out" | grep -v '^$')"
  [ -n "$out" ] && printf '%s\n' "$out" | sed "s|^|$f:|"
}

echo "=== árbol de trabajo (archivos trackeados) ==="
while IFS= read -r f; do
  [ -f "$f" ] || continue
  # El propio scanner lleva los patrones como texto (y está vetado a mano): no se
  # auto-escanea. Si se versiona una copia nueva del scanner, agregarla acá.
  [ "$f" = "scripts/scan-leaks.sh" ] && continue
  reportar "$(scan_archivo "$f")"
done < <(git ls-files)

if [ "$MODE" != "--worktree" ]; then
  echo
  echo "=== historia (todos los commits) ==="
  # El paso que siempre se saltea: un pre-commit solo mira lo staged; lo ya
  # commiteado queda en el object store y sale con el próximo push. Si el repo
  # NUNCA se pusheó, no hace falta reescribir nada: el primer push limpio se
  # arma con `git archive` del árbol saneado + commit raíz (sin historia vieja).
  #
  # Mismas exclusiones por línea que el modo árbol: los CIDR de EJEMPLO del nginx
  # y del propio comentario del scanner no identifican a nadie (bug cazado acá:
  # el modo historia no las aplicaba y marcaba falsos positivos).
  historial_patron="$HOME_PATHS|$REDES_PRIVADAS"
  hist_salida="$(
    while IFS= read -r c; do
      {
        git grep -I -n -i -E "$historial_patron" "$c" -- . 2>/dev/null \
          | grep -vF -e '10.0.0.0/8' -e '172.16.0.0/12' -e '192.168.0.0/16'
        if [ -n "$PATTERNS_PERSONAL" ]; then
          git grep -I -n -i -E "$PATTERNS_PERSONAL" "$c" -- . 2>/dev/null
        fi
      } | while IFS= read -r linea; do
        [ -n "$linea" ] || continue
        resto="${linea#"$c":}"   # git grep con un rev PREFIJA `rev:`
        printf '  %s:%s\n' "$c" "${resto%:*}"   # archivo:línea (sin el texto)
      done
    done < <(git rev-list --all) | sort -u
  )"
  if [ -z "$hist_salida" ]; then
    echo "  (limpio)"
    hist=0
  else
    printf '%s\n' "$hist_salida"
    hist="$(printf '%s\n' "$hist_salida" | grep -c .)"
    echo "  $hist coincidencia(s) en la historia."
  fi
  hits=$((hits + hist))
fi

echo
if [ "$hits" -eq 0 ]; then
  echo "OK: nada que filtrar."
  exit 0
fi
echo "$hits hallazgo(s)."
exit 1

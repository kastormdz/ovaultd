#!/usr/bin/env bash
# bench-lan.sh — Benchmark LAN (fase 5, paso 6): N notas de ~KB KB contra un server real.
#
# No levanta server: usa uno YA ARRIBA (BENCH_BASE, default el lab de desarrollo :8894).
# Todo el estado del CLI vive en .tmp/bench (gitignored): nada toca la config del usuario.
# Credenciales SIEMPRE por entorno (jamás en el repo; el scanner de leaks lo vigila):
#
#   BENCH_EMAIL / BENCH_PASSWORD   cuenta del server (la password va por stdin/env)
#   BENCH_VAULT_PASSWORD           password del vault de prueba (se inventa)
#   BENCH_BASE (default http://127.0.0.1:8894) · BENCH_NOTAS (1000) · BENCH_KB (100)
#   BENCH_LIMPIAR=0                deja el vault en el server (default: lo borra al final)
#
# Mide: T1 primera subida (vault nuevo), T2 ciclo en reposo, T3 un cambio suelto y
# T4 la bajada completa en un segundo cliente. Imprime la tabla; los números publicados
# viven en docs/benchmarks.md.
set -euo pipefail

RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$RAIZ"

D="${BENCH_DIR:-$RAIZ/.tmp/bench}"
BASE="${BENCH_BASE:-http://127.0.0.1:8894}"
NOTAS="${BENCH_NOTAS:-1000}"
KB="${BENCH_KB:-100}"

: "${BENCH_EMAIL:?falta BENCH_EMAIL (cuenta del server)}"
: "${BENCH_PASSWORD:?falta BENCH_PASSWORD (de la cuenta)}"
: "${BENCH_VAULT_PASSWORD:?falta BENCH_VAULT_PASSWORD (del vault de prueba)}"

CLI=(node "$RAIZ/packages/cli/dist/main.js")
export OVAULTD_CONFIG_DIR="$D/config"
# Cada cliente (config + runtime) va AISLADO en el bench: el runtime dir por defecto es
# GLOBAL del usuario (~/.local/share/ovaultd/runtime) y un segundo cliente que lo comparta
# hereda el checkpoint del primero — el "vault nuevo" arranca en el head ajeno y su scan
# sube borrados en vez de bajar (cazado re-corriendo este script).
export OVAULTD_RUNTIME_DIR="$D/runtime"

paso() { printf '\n== %s\n' "$*"; }
nota() { printf '   %s\n' "$*"; }
fallo() {
    printf '\nFALLO: %s\n' "$*" >&2
    exit 1
}

# Corre "$@" con la password del vault por entorno y devuelve los ms por stdout.
# El output del comando queda en $D/ultimo.log (se muestra el tail si algo falla).
medir_ms() {
    local t0 t1 rc
    t0=$(date +%s%N)
    OVAULTD_PASSWORD="$BENCH_VAULT_PASSWORD" "$@" >"$D/ultimo.log" 2>&1
    rc=$?
    t1=$(date +%s%N)
    echo "$(( (t1 - t0) / 1000000 ))"
    return $rc
}

paso "server vivo ($BASE)"
curl -sf "$BASE/api/v1/meta" | head -c 120 || fallo "el server no responde en $BASE (¿está arriba?)"
echo

paso "terreno fresco en $D"
rm -rf "$D"
mkdir -p "$D/vault/Notas" "$D/vault2" "$D/runtime" "$D/runtime2"

paso "login + vault nuevo de $NOTAS notas de ~${KB} KB"
echo "$BENCH_PASSWORD" | "${CLI[@]}" login "$BASE" --email "$BENCH_EMAIL" --password-stdin | tail -1
NOMBRE="bench-${NOTAS}x${KB}k"
echo "$BENCH_VAULT_PASSWORD" | "${CLI[@]}" create-remote "$NOMBRE" --password-stdin --recovery-out "$D/recovery.txt" >"$D/ultimo.log" 2>&1
VID="$(sed -n 's/.*#\([0-9][0-9]*\).*/\1/p' "$D/ultimo.log" | head -1)"
[ -n "$VID" ] || fallo "no pude leer el id del vault creado"
nota "vault #$VID «$NOMBRE»"
grep -o "vault creado.*" "$D/ultimo.log" || true

paso "genero las notas (~${KB}000 B c/u)"
python3 - "$NOTAS" "$KB" "$D/vault/Notas" <<'PY'
import os
import sys

notas, kb, destino = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3]
objetivo = kb * 1000
# Párrafos de 100 B exactos: el archivo queda a ±1 párrafo del objetivo y SIEMPRE es
# UTF-8 válido (nada de cortar a mitad de un char multibyte).
parrafo = ("Texto de relleno para el benchmark LAN de ovaultd; se repite con un "
           "patron deterministico para que los hashes sean estables.\n\n")
parrafo = parrafo[:98] + "\n\n"
assert len(parrafo.encode()) == 100
for i in range(notas):
    cuerpo = f"# Nota {i:04d}\n\n"
    while len(cuerpo.encode()) < objetivo:
        cuerpo += parrafo
    with open(os.path.join(destino, f"n-{i:04d}.md"), "w", encoding="utf-8") as f:
        f.write(cuerpo)
PY
BYTES="$(du -sb "$D/vault" | awk '{print $1}')"
nota "datos: $BYTES B ($(awk -v b="$BYTES" 'BEGIN{printf "%.1f", b/1000000}') MB)"

echo "$BENCH_VAULT_PASSWORD" | "${CLI[@]}" setup "$VID" --dir "$D/vault" --password-stdin >/dev/null
nota "vault vinculado a $D/vault"

paso "T1 · primera subida (vault nuevo: $NOTAS altas)"
T1="$(medir_ms "${CLI[@]}" sync "$VID")" || { tail -5 "$D/ultimo.log"; fallo "T1"; }
tail -1 "$D/ultimo.log"
MBPS="$(awk -v b="$BYTES" -v ms="$T1" 'BEGIN{printf "%.2f", (b/1000000)/(ms/1000)}')"
nota "T1 = ${T1} ms (${MBPS} MB/s)"

paso "T2 · ciclo en reposo (sin cambios)"
T2="$(medir_ms "${CLI[@]}" sync "$VID")" || { tail -5 "$D/ultimo.log"; fallo "T2"; }
tail -1 "$D/ultimo.log"
nota "T2 = ${T2} ms"

paso "T3 · un cambio suelto (1 de $NOTAS notas)"
printf '# Nota 0000\n\neditada por el benchmark\n' > "$D/vault/Notas/n-0000.md"
T3="$(medir_ms "${CLI[@]}" sync "$VID")" || { tail -5 "$D/ultimo.log"; fallo "T3"; }
tail -1 "$D/ultimo.log"
nota "T3 = ${T3} ms"

paso "T4 · bajada completa en un segundo cliente"
export OVAULTD_CONFIG_DIR="$D/config2"
export OVAULTD_RUNTIME_DIR="$D/runtime2"
echo "$BENCH_PASSWORD" | "${CLI[@]}" login "$BASE" --email "$BENCH_EMAIL" --password-stdin >/dev/null
echo "$BENCH_VAULT_PASSWORD" | "${CLI[@]}" setup "$VID" --dir "$D/vault2" --password-stdin >/dev/null
T4="$(medir_ms "${CLI[@]}" sync "$VID")" || { tail -5 "$D/ultimo.log"; fallo "T4"; }
tail -1 "$D/ultimo.log"
# La bajada es BAJADA: si el cliente nuevo sube algo, el runtime no estaba aislado
# (o el vault quedó sucio) — abortar antes de dejar el server mutado.
grep -q "↑ 0 subida(s)" "$D/ultimo.log" || { tail -3 "$D/ultimo.log"; fallo "el 2º cliente subió algo en la bajada"; }
BAJADAS="$(find "$D/vault2/Notas" -name '*.md' | wc -l)"
[ "$BAJADAS" = "$NOTAS" ] || fallo "bajaron $BAJADAS archivos, esperaba $NOTAS"
MBPS2="$(awk -v b="$BYTES" -v ms="$T4" 'BEGIN{printf "%.2f", (b/1000000)/(ms/1000)}')"
nota "T4 = ${T4} ms ($BAJADAS archivos, ${MBPS2} MB/s)"
export OVAULTD_CONFIG_DIR="$D/config"
export OVAULTD_RUNTIME_DIR="$D/runtime"

paso "resultados"
printf '  notas: %s · datos: %s B\n' "$NOTAS" "$BYTES"
printf '  T1 primera subida:      %6s ms\n' "$T1"
printf '  T2 ciclo en reposo:     %6s ms\n' "$T2"
printf '  T3 un cambio suelto:    %6s ms\n' "$T3"
printf '  T4 bajada (2º cliente): %6s ms\n' "$T4"

if [ "${BENCH_LIMPIAR:-1}" != "0" ]; then
    paso "limpieza: borro el vault #$VID del server"
    TOKEN="$(jq -r .token "$D/config/session.json")"
    curl -sf -X DELETE "$BASE/api/v1/vaults/$VID?confirm=$NOMBRE" -H "authorization: Bearer $TOKEN" >/dev/null \
        && nota "vault borrado" || nota "no se pudo borrar (dejado a propósito)"
fi

printf '\nBENCH OK\n'

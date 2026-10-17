#!/usr/bin/env bash
# backup.sh — backup operacional de ovaultd (fase 2, paso 5).
#
# La alternativa simple permitida por el plan: backup con el servidor DETENIDO.
# Quietud total ⇒ la base se copia con la API de backup de SQLite (consistente por
# definición, WAL incluido) y el CAS se archiva tal cual. El original NUNCA se toca:
# solo lectura + un staging temporal.
#
# Uso:
#   backup.sh --db <metadata.db> --cas <dir-cas> --output <archivo.tar.zst>
#             [--stop-cmd "<comando>"] [--start-cmd "<comando>"]
#
# --stop-cmd/--start-cmd: hooks para detener/levantar el servidor alrededor del backup
# (p. ej. `docker compose stop ovaultd` / `docker compose start ovaultd`). El start se
# ejecuta SIEMPRE, también si el backup falló: un backup no puede dejar el servicio
# apagado. Sin hooks se asume quietud (backup en frío manual).
#
# El archivo lleva un `manifest.json` con checksums sha256 de la base y de cada blob,
# más los conteos de la base; `restore.sh` los verifica TODOS antes de dar por buena
# una restauración.
#
# Salida: <output> + <output>.sha256. Exit != 0 si algo falló (el start igual corre).
set -euo pipefail

DB=""
CAS=""
OUTPUT=""
STOP_CMD=""
START_CMD=""

uso() {
    sed -n '2,18p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --db) DB="${2:?}"; shift 2 ;;
        --cas) CAS="${2:?}"; shift 2 ;;
        --output) OUTPUT="${2:?}"; shift 2 ;;
        --stop-cmd) STOP_CMD="${2:?}"; shift 2 ;;
        --start-cmd) START_CMD="${2:?}"; shift 2 ;;
        -h|--help) uso ;;
        *) echo "backup: argumento desconocido: $1" >&2; uso ;;
    esac
done

[ -n "$DB" ] && [ -n "$CAS" ] && [ -n "$OUTPUT" ] || uso
[ -f "$DB" ] || { echo "backup: no existe la base: $DB" >&2; exit 1; }
[ -d "$CAS" ] || { echo "backup: no existe el CAS: $CAS" >&2; exit 1; }
[ -n "$STOP_CMD" ] && [ -z "$START_CMD" ] && { echo "backup: --stop-cmd sin --start-cmd" >&2; exit 2; }

STAGING="$(mktemp -d "${TMPDIR:-/tmp}/ovaultd-backup.XXXXXX")"
REINICIAR=0

limpiar() {
    rm -rf "$STAGING"
    # El servicio se levanta SIEMPRE que lo hayamos bajado, aunque el backup haya
    # fallado a mitad: el fallo del backup no puede convertirse en caída del servicio.
    if [ "$REINICIAR" = "1" ]; then
        echo "   levantando de nuevo: $START_CMD"
        eval "$START_CMD" || echo "backup: OJO: el start-cmd falló; levantá el servicio a mano" >&2
    fi
}
trap limpiar EXIT

paso() { printf '\n== %s\n' "$*"; }
nota() { printf '   %s\n' "$*"; }

paso "deteniendo el servidor"
if [ -n "$STOP_CMD" ]; then
    eval "$STOP_CMD"
    REINICIAR=1
    nota "detenido: $STOP_CMD"
else
    nota "sin hooks: se asume quietud (nadie escribiendo)"
fi

paso "copia consistente de la base (API de backup de SQLite)"
python3 - "$DB" "$STAGING/metadata.db" <<'PY'
import sqlite3, sys

origen, destino = sys.argv[1], sys.argv[2]
con = sqlite3.connect(f"file:{origen}?mode=ro", uri=True)
dst = sqlite3.connect(destino)
with dst:
    con.backup(dst)   # API de backup: consistente incluso con WAL
dst.execute("PRAGMA journal_mode = DELETE")  # el backup viaja sin WAL pendiente
dst.close()
con.close()
print("   base copiada")
PY

paso "archivando el CAS"
mkdir -p "$STAGING/cas"
# Los .tmp-* son temporales huérfanos de publicaciones interrumpidas (nada los
# referencia); no van al backup, se avisa cuántos quedaron afuera.
TEMPORALES="$(find "$CAS" -type f -name '.tmp-*' | wc -l)"
if [ "$TEMPORALES" -gt 0 ]; then
    nota "$TEMPORALES temporal(es) huérfano(s) excluidos (basura recuperable, no referenciada)"
fi
find "$CAS" -type f -name '.tmp-*' -prune -o -type f -print0 \
    | while IFS= read -r -d '' archivo; do
        rel="${archivo#"$CAS"/}"
        mkdir -p "$STAGING/cas/$(dirname "$rel")"
        cp -p "$archivo" "$STAGING/cas/$rel"
    done
BLOBS="$(find "$STAGING/cas" -type f | wc -l)"
nota "$BLOBS blob(s) en el archivo"

paso "manifiesto con checksums"
python3 - "$STAGING" <<'PY'
import hashlib, json, os, sqlite3, sys

staging = sys.argv[1]

def sha256(ruta):
    h = hashlib.sha256()
    with open(ruta, "rb") as f:
        for bloque in iter(lambda: f.read(1 << 20), b""):
            h.update(bloque)
    return h.hexdigest()

cas = os.path.join(staging, "cas")
blobs = []
for raiz, _, archivos in os.walk(cas):
    for nombre in sorted(archivos):
        ruta = os.path.join(raiz, nombre)
        rel = os.path.relpath(ruta, cas)
        blobs.append({"ruta": rel, "sha256": sha256(ruta), "size": os.path.getsize(ruta)})

con = sqlite3.connect(os.path.join(staging, "metadata.db"))
def contar(tabla):
    return con.execute(f"SELECT count(*) FROM {tabla}").fetchone()[0]
conteos = {t: contar(t) for t in ("vaults", "revisions", "files", "blobs", "batches")}
con.close()

manifest = {
    "tool": "ovaultd backup.sh",
    "created_at": __import__("datetime").datetime.now().astimezone().isoformat(timespec="seconds"),
    "db": {"archivo": "metadata.db", "sha256": sha256(os.path.join(staging, "metadata.db"))},
    "cas": {"total": len(blobs), "blobs": blobs},
    "conteos": conteos,
}
with open(os.path.join(staging, "manifest.json"), "w") as f:
    json.dump(manifest, f, indent=2, sort_keys=True)

print(f"   db sha256={manifest['db']['sha256'][:16]}…  blobs={len(blobs)}  conteos={conteos}")
PY

paso "empaquetando"
rm -f "$OUTPUT" "$OUTPUT.sha256"
mkdir -p "$(dirname "$OUTPUT")"
tar -C "$STAGING" -cf - . | zstd -19 -T0 -q -o "$OUTPUT"
( cd "$(dirname "$OUTPUT")" && sha256sum "$(basename "$OUTPUT")" > "$(basename "$OUTPUT").sha256" )
nota "archivo: $OUTPUT ($(du -h "$OUTPUT" | cut -f1))"
nota "checksum: $(cut -d' ' -f1 "$OUTPUT.sha256")"

paso "backup OK"

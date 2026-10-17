#!/usr/bin/env bash
# restore.sh — restauración verificada de un backup de ovaultd (fase 2, paso 5).
#
# Verifica ANTES de tocar el destino: nada se restaura si un checksum no cierra.
#
# Uso:
#   restore.sh --input <archivo.tar.zst> --into <dir> [--force] [--project <nombre>]
#
#   --into     directorio destino. Si existe y NO está vacío, se rechaza salvo --force
#              (un restore JAMÁS pisa datos existentes en silencio).
#   --force    permite escribir en un destino no vacío (se sobreescribe archivo por
#              archivo; el directorio no se borra).
#   --project  nombre del proyecto Docker para el stack de verificación sugerido
#              (el restore NO levanta nada: imprime el comando; ver docs/runbook.md).
#
# Qué verifica, en orden:
#   1. sha256 del archivo (si existe `<input>.sha256` — lo escribe backup.sh).
#   2. CADA entrada del manifiesto: sha256 de la base y de TODOS los blobs.
#   3. Integridad SQL: `PRAGMA integrity_check` y `foreign_key_check`.
#   4. Referencias: toda fila de `blobs` tiene su archivo en el CAS restaurado, y todo
#      `blob_hash` vivo (files/revisions) tiene su fila. Los archivos del CAS sin fila
#      son huérfanos recuperables: se informan, no invalidan el backup.
#
# Al terminar imprime el resumen (árbol, historial, wrappers y firmas) y el comando del
# stack de verificación. Exit != 0 en el primer chequeo que falle; el destino queda
# intacto (todo pasa en un staging temporal antes del copiado).
set -euo pipefail

INPUT=""
INTO=""
FORCE=0
PROJECT="ovaultd-restore-check"

uso() {
    sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --input) INPUT="${2:?}"; shift 2 ;;
        --into) INTO="${2:?}"; shift 2 ;;
        --force) FORCE=1; shift ;;
        --project) PROJECT="${2:?}"; shift 2 ;;
        -h|--help) uso ;;
        *) echo "restore: argumento desconocido: $1" >&2; uso ;;
    esac
done

[ -n "$INPUT" ] && [ -n "$INTO" ] || uso
[ -f "$INPUT" ] || { echo "restore: no existe el backup: $INPUT" >&2; exit 1; }
case "$PROJECT" in *[!a-zA-Z0-9_.-]*) echo "restore: --project inválido: $PROJECT" >&2; exit 2 ;; esac

STAGING="$(mktemp -d "${TMPDIR:-/tmp}/ovaultd-restore.XXXXXX")"
trap 'rm -rf "$STAGING"' EXIT

paso() { printf '\n== %s\n' "$*"; }
nota() { printf '   %s\n' "$*"; }
fallo() { printf '\nFALLO: %s\n' "$*" >&2; exit 1; }

paso "verificando el archivo"
if [ -f "$INPUT.sha256" ]; then
    ( cd "$(dirname "$INPUT")" && sha256sum -c "$(basename "$INPUT").sha256" --quiet ) \
        || fallo "el sha256 del archivo no coincide con $INPUT.sha256"
    nota "sha256 del archivo: OK"
else
    nota "sin sidecar .sha256 (se verifica igual todo el contenido del manifiesto)"
fi

paso "destino"
if [ -e "$INTO" ] && [ -n "$(ls -A "$INTO" 2>/dev/null)" ]; then
    [ "$FORCE" = "1" ] || fallo "el destino NO está vacío: $INTO (usá --force para sobreescribir)"
    nota "destino no vacío + --force: se sobreescribe archivo por archivo"
else
    nota "destino vacío o inexistente: $INTO"
fi

paso "extrayendo a staging y verificando el manifiesto"
mkdir -p "$STAGING"
zstd -dc "$INPUT" | tar -C "$STAGING" -xf -
[ -f "$STAGING/manifest.json" ] || fallo "el backup no trae manifest.json"

python3 - "$STAGING" <<'PY' || fallo "la verificación del manifiesto falló"
import hashlib, json, os, sys

staging = sys.argv[1]
manifest = json.load(open(os.path.join(staging, "manifest.json")))

def sha256(ruta):
    h = hashlib.sha256()
    with open(ruta, "rb") as f:
        for bloque in iter(lambda: f.read(1 << 20), b""):
            h.update(bloque)
    return h.hexdigest()

db = os.path.join(staging, manifest["db"]["archivo"])
real = sha256(db)
if real != manifest["db"]["sha256"]:
    print(f"   FALLO: la base no coincide (esperado {manifest['db']['sha256'][:16]}…, real {real[:16]}…)")
    sys.exit(1)
print("   base: sha256 OK")

verificados = 0
for blob in manifest["cas"]["blobs"]:
    ruta = os.path.join(staging, "cas", blob["ruta"])
    if not os.path.isfile(ruta):
        print(f"   FALLO: falta el blob {blob['ruta']}")
        sys.exit(1)
    if os.path.getsize(ruta) != blob["size"] or sha256(ruta) != blob["sha256"]:
        print(f"   FALLO: el blob {blob['ruta']} no coincide")
        sys.exit(1)
    verificados += 1
print(f"   blobs: {verificados}/{len(manifest['cas']['blobs'])} sha256 OK")
PY

paso "integridad SQL y referencias CAS"
python3 - "$STAGING" <<'PY' || fallo "la verificación de integridad falló"
import os, sqlite3, sys

staging = sys.argv[1]
cas = os.path.join(staging, "cas")
con = sqlite3.connect(os.path.join(staging, "metadata.db"))

integridad = con.execute("PRAGMA integrity_check").fetchone()[0]
if integridad != "ok":
    print(f"   FALLO: integrity_check = {integridad}")
    sys.exit(1)
print("   integrity_check: ok")

fks = con.execute("PRAGMA foreign_key_check").fetchall()
if fks:
    print(f"   FALLO: foreign_key_check devolvió {len(fks)} fila(s)")
    sys.exit(1)
print("   foreign_key_check: limpio")

# Referencia crítica: cada fila de `blobs` TIENE que tener su archivo en el CAS.
filas = con.execute("SELECT vault_id, hash FROM blobs").fetchall()
faltantes = []
for vault_id, hash_bytes in filas:
    hexh = hash_bytes.hex()
    if not os.path.isfile(os.path.join(cas, hexh[:2], hexh)):
        faltantes.append((vault_id, hexh))
if faltantes:
    print(f"   FALLO: {len(faltantes)} blob(s) del índice sin archivo (ej. {faltantes[0][1][:16]}…)")
    sys.exit(1)
print(f"   referencias índice→CAS: {len(filas)}/{len(filas)} OK")

# blob_hash vivos (files/revisions) con fila en el índice: consistencia interna.
sueltos = con.execute(
    """SELECT count(*) FROM (
         SELECT blob_hash AS h FROM files WHERE blob_hash IS NOT NULL AND deleted = 0
         UNION
         SELECT blob_hash FROM revisions WHERE blob_hash IS NOT NULL
       ) WHERE h NOT IN (SELECT hash FROM blobs)"""
).fetchone()[0]
if sueltos:
    print(f"   FALLO: {sueltos} blob_hash vivo(s) sin fila en blobs")
    sys.exit(1)

# Huérfanos del CAS (archivo sin fila): basura recuperable, solo se informa.
archivos = set()
for raiz, _, nombres in os.walk(cas):
    for nombre in nombres:
        archivos.add(nombre)
con_fila = {hash_bytes.hex() for _, hash_bytes in filas}
huerfanos = archivos - con_fila
print(f"   huérfanos en el CAS (informativos): {len(huerfanos)}")
PY

paso "copiando al destino"
mkdir -p "$INTO"
cp -a "$STAGING/." "$INTO/"
nota "restaurado en: $INTO"

paso "resumen"
python3 - "$INTO" <<'PY'
import os, sqlite3, sys

into = sys.argv[1]
con = sqlite3.connect(os.path.join(into, "metadata.db"))
def una(sql):
    return con.execute(sql).fetchone()[0]

print(f"   vaults={una('SELECT count(*) FROM vaults')}"
      f"  (con wrappers: {una('SELECT count(*) FROM vaults WHERE wrapped_key_pw IS NOT NULL AND wrapped_key_rc IS NOT NULL')},"
      f" con pubkey: {una('SELECT count(*) FROM vaults WHERE vault_pubkey IS NOT NULL')})")
print(f"   árbol (files vivos)={una('SELECT count(*) FROM files WHERE deleted = 0')}"
      f"  tombstones={una('SELECT count(*) FROM files WHERE deleted = 1')}")
print(f"   historial (revisions)={una('SELECT count(*) FROM revisions')}"
      f"  (con firma: {una('SELECT count(*) FROM revisions WHERE sig IS NOT NULL')})")
print(f"   blobs (índice)={una('SELECT count(*) FROM blobs')}")
con.close()
PY

cat <<EOF

== restore OK
Para verificar con un stack aparte (SIN tocar el actual):
  1. apuntá OVAULTD_DATA (o el bind del compose) a: $INTO
  2. docker compose -p $PROJECT up -d
  3. curl -fsS http://127.0.0.1:8080/healthz
  4. si verifica: bajá ambos stacks y apuntá el servicio real al volumen restaurado
     (procedimiento completo en docs/runbook.md)
EOF

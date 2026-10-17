#!/usr/bin/env bash
# Spike de interop cripto `sync2` (fase 0.2).
#   ./run.sh                        corre Rust y JS y diffea los vectores (no toca fixtures)
#   ./run.sh --check                ademas valida contra tests/fixtures/crypto/sync2.json
#   ./run.sh --emit-candidates DIR  escribe DIR/{rust,js}.json para revisar ANTES de congelar
#   ./run.sh --freeze               regenera el fixture (solo tras revisar los candidatos)
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
FIXTURE="$REPO/tests/fixtures/crypto/sync2.json"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/cargo-target/ovaultd}"

echo "== Rust ==" >&2
( cd "$HERE/rust" && cargo run --release --quiet ) > "$HERE/.out_rust.txt"
echo "== JS ==" >&2
( cd "$HERE/js" && { [ -d node_modules ] || npm install --silent; } && node spike.mjs ) > "$HERE/.out_js.txt"

case "${1:-run}" in
  --streaming)    ( cd "$HERE/js" && node streaming.mjs ) ;;
  --check) python3 "$HERE/compare.py" --check "$HERE/.out_rust.txt" "$HERE/.out_js.txt" "$FIXTURE" ;;
  --freeze) python3 "$HERE/freeze.py" --from "$HERE/.out_rust.txt" --json "$FIXTURE" ;;
  --emit-candidates)
      DEST="${2:?falta el directorio destino}"
      mkdir -p "$DEST"
      python3 "$HERE/freeze.py" --from "$HERE/.out_rust.txt" --json "$DEST/rust.json" --source-label "spike (rust)"
      python3 "$HERE/freeze.py" --from "$HERE/.out_js.txt"   --json "$DEST/js.json"   --source-label "spike (js)"
      echo "candidatos escritos en $DEST: revisar y despues correr --freeze" >&2 ;;
  *) python3 "$HERE/compare.py" "$HERE/.out_rust.txt" "$HERE/.out_js.txt" ;;
esac

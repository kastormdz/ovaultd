#!/usr/bin/env bash
# Preflight de release del server (ADR-0018): el tag ES la versión declarada.
#
#   bash scripts/preflight-release.sh server-v0.2.0
#
# Falla si el tag no tiene la forma `server-vX.Y.Z`, si la versión del crate del server no
# coincide con el tag, o si los tres crates no comparten versión (release train interno).
# El workflow de release lo corre ANTES de compilar: un tag mal puesto se cae acá y no
# publica nada a medias.
set -euo pipefail

tag="${1:?uso: bash scripts/preflight-release.sh server-vX.Y.Z}"
raiz="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "$tag" in
  server-v*) esperado="${tag#server-v}" ;;
  *)
    echo "✗ el tag tiene que ser server-vX.Y.Z (recibí: $tag)" >&2
    exit 1
    ;;
esac

if ! printf '%s' "$esperado" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "✗ '$esperado' no es una versión X.Y.Z" >&2
  exit 1
fi

version_de() { sed -n 's/^version = "\(.*\)"/\1/p' "$1" | head -1; }

server="$(version_de "$raiz/crates/ovaultd-server/Cargo.toml")"
crypto="$(version_de "$raiz/crates/ovaultd-crypto/Cargo.toml")"
protocol="$(version_de "$raiz/crates/ovaultd-protocol/Cargo.toml")"

if [ "$server" != "$esperado" ]; then
  echo "✗ el tag dice $esperado y crates/ovaultd-server/Cargo.toml dice $server" >&2
  exit 1
fi

for par in "ovaultd-crypto:$crypto" "ovaultd-protocol:$protocol"; do
  nombre="${par%%:*}"
  version="${par#*:}"
  if [ "$version" != "$esperado" ]; then
    echo "✗ el tag dice $esperado y crates/$nombre dice $version (el tren va junto)" >&2
    exit 1
  fi
done

echo "✓ server-v$esperado: tag y los tres crates coinciden"

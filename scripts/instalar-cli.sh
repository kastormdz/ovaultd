#!/usr/bin/env bash
# instalar-cli.sh — instala el comando `ovaultd-cli` en el PATH (~/.local/bin por defecto)
# apuntando a ESTE checkout del monorepo. El paquete @ovaultd/cli es privado (no va al
# registry): el comando es un shim que ejecuta el dist compilado con node.
#
#   bash scripts/instalar-cli.sh            # ~/.local/bin/ovaultd-cli
#   OVAULTD_BIN_DIR=/usr/local/bin bash scripts/instalar-cli.sh
#
# Re-ejecutable: sobreescribe el shim (y no toca nada más).
set -euo pipefail

RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DESTINO="${OVAULTD_BIN_DIR:-$HOME/.local/bin}"
MAIN="$RAIZ/packages/cli/dist/main.js"

if [ ! -f "$MAIN" ]; then
    echo "falta el dist del CLI: corré primero 'npm run build -w @ovaultd/cli' en $RAIZ" >&2
    exit 1
fi

mkdir -p "$DESTINO"
cat > "$DESTINO/ovaultd-cli" <<EOF
#!/usr/bin/env bash
# Generado por scripts/instalar-cli.sh — apunta al checkout $RAIZ
set -euo pipefail
exec node --disable-warning=ExperimentalWarning "$MAIN" "\$@"
EOF
chmod +x "$DESTINO/ovaultd-cli"

echo "instalado: $DESTINO/ovaultd-cli -> $RAIZ"
case ":$PATH:" in
    *":$DESTINO:"*) echo "en PATH: $(command -v ovaultd-cli)" ;;
    *) echo "OJO: $DESTINO no está en PATH; agregalo a tu shell" ;;
esac

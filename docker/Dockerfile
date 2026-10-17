# syntax=docker/dockerfile:1
#
# ovaultd — imagen del servidor.
#
# - Runtime non-root (uid/gid 10001), pensada para `read_only: true` (solo escribe /data).
# - Las migraciones van EMBEBIDAS en el binario (`sqlx::migrate!`), así que la imagen final
#   no las copia: migrar en un volumen existente es automático al arrancar.
# - Healthcheck propio contra /healthz.
#
# Build de desarrollo (recomendado):
#   docker compose -f compose.yaml -f docker/compose.dev.yaml build
# Build suelto:
#   docker build -f docker/Dockerfile -t ovaultd:local .

FROM rust:1.99-bookworm AS builder
WORKDIR /src

# ---- Capa de dependencias (se cachea mientras no cambien Cargo.toml/Cargo.lock) -----------
# Los stubs dejan que cargo compile TODO el grafo de dependencias (tokio, axum, sqlx, argon2…)
# antes de copiar el código real: editar un .rs recompila solo los crates del workspace, no
# las ~400 dependencias. El `|| true` es por si el stub no llega a enlazar: lo que importa
# es que las deps quedaron compiladas en target/.
COPY Cargo.toml Cargo.lock ./
COPY crates/ovaultd-crypto/Cargo.toml crates/ovaultd-crypto/
COPY crates/ovaultd-protocol/Cargo.toml crates/ovaultd-protocol/
COPY crates/ovaultd-server/Cargo.toml crates/ovaultd-server/
COPY xtask/Cargo.toml xtask/
RUN set -eu; \
    for c in ovaultd-crypto ovaultd-protocol; do \
        mkdir -p "crates/$c/src"; \
        echo '' > "crates/$c/src/lib.rs"; \
    done; \
    mkdir -p crates/ovaultd-server/src xtask/src; \
    echo '' > crates/ovaultd-server/src/lib.rs; \
    printf 'fn main() {}\n' > crates/ovaultd-server/src/main.rs; \
    printf 'fn main() {}\n' > xtask/src/main.rs; \
    cargo build --release --locked --package ovaultd-server --bin ovaultd || true

# ---- Código real -------------------------------------------------------------------------
# Los fuentes del contexto llegan con mtime VIEJO y el layer de stubs ya dejó artefactos para
# esos mismos paths: sin el `touch`, cargo los considera frescos y la imagen se queda con el
# binario stub (exit 0, sin logs — lo cazó el healthcheck del E2E). El touch recompila SOLO
# los crates del workspace; las dependencias siguen cacheadas por huella.
COPY . .
RUN find crates xtask -name '*.rs' -exec touch {} + \
 && cargo build --release --locked --package ovaultd-server --bin ovaultd --bin ovaultd-admin

FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --gid 10001 ovaultd \
 && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin ovaultd \
 && install -d -o ovaultd -g ovaultd /data \
 && install -d -o ovaultd -g ovaultd /data/cas

COPY --from=builder /src/target/release/ovaultd /usr/local/bin/ovaultd
COPY --from=builder /src/target/release/ovaultd-admin /usr/local/bin/ovaultd-admin

# Defaults de contenedor: escucha en todas las interfaces (adelante está el proxy) y todo el
# estado bajo /data (volumen). Overridables por env, como cualquier otra variable.
ENV OVAULTD_BIND=0.0.0.0:8080 \
    OVAULTD_DB=/data/metadata.db \
    OVAULTD_CAS=/data/cas \
    OVAULTD_LOG_JSON=1

USER 10001:10001
EXPOSE 8080
VOLUME ["/data"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1

ENTRYPOINT ["/usr/local/bin/ovaultd"]

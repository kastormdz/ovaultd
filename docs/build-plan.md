# Plan de construcción — servidor y plugin

## Alcance y reglas de ejecución

Este plan construye el MVP desktop de `ovaultd`: servidor Rust de un nodo, SQLite WAL, CAS en filesystem, cliente durable, CLI y plugin de Obsidian. **No vuelve a diseñar el protocolo ni la cripto.**

Fuentes operativas:

- `docs/crypto.md`: cripto `sync2`.
- `docs/protocol.md`: protocolo v2.
- `docs/decisions/0001-stack-y-alcance.md` a `0005-protocolo-v2.md`: decisiones aceptadas.
- `migrations/001_init.sql`: esquema SQLite existente.
- La especificación canónica: requisitos y comportamiento del producto.

Los archivos existentes se conservan. No se modifica una migración aplicada ni se regenera un vector dorado para “hacer pasar” un test.

El inventario recibido no incluye el contenido completo de los documentos del repositorio. Por eso, el primer paso verifica sus detalles —serialización firmada, tablas auxiliares, secuencia de creación de vault— antes de codificarlos. Los nombres nuevos de archivos de este plan son propuestas de implementación; no afirmaciones sobre código existente.

> ⚠️ DISCREPANCIA: los ejemplos antiguos con `proto: 1`, `t` duplicado, proofs HMAC y `CryptoProvider` passthrough quedaron superados; se implementan exclusivamente protocolo v2, firmas Ed25519 y cifrado real.

> ⚠️ DISCREPANCIA: la estimación vieja de 6–8 semanas no cubre el MVP durable; este plan estima 20–30 semanas para un desarrollador solo.

### Presupuesto

| Fase | Semanas |
|---|---:|
| 0.2 — Gateway `sync2` | 1–2 |
| 1 — Servidor MVP | 4–5 |
| 2 — Cliente durable y recuperación operacional | 4–6 |
| 3 — Plugin y sincronización real | 5–6 |
| 4 — Ciclo de vida de claves | 3–4 |
| 5 — Historia, restore y conflictos finos | 3–5 |
| Subtotal | **20–28** |
| Reserva transversal | **0–2** |
| Total | **20–30** |

La estimación incluye tests, documentación operativa y correcciones. No incluye móvil, sharing, publicación en el directorio ni una auditoría criptográfica externa con plazo propio.

Todos los comandos se ejecutan desde la raíz, salvo indicación contraria. Los scripts nuevos se implementan en la fase que los introduce: no existen todavía.

---

## 0. Estructura del monorepo

```text
ovaultd/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── .cargo/
│   └── config.toml
├── package.json
├── package-lock.json
├── tsconfig.base.json
├── eslint.config.mjs
├── vitest.config.ts
├── .env.example
├── compose.yaml
├── README.md
├── LICENSE
├── .github/workflows/
│   └── ci.yml
├── crates/
│   ├── ovaultd-protocol/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── messages.rs
│   │       └── canonical.rs
│   ├── ovaultd-crypto/
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── verify.rs
│   │   │   └── interop.rs
│   │   └── tests/
│   │       └── sync2_vectors.rs
│   └── ovaultd-server/
│       ├── Cargo.toml
│       ├── src/
│       │   ├── main.rs
│       │   ├── lib.rs
│       │   ├── config.rs
│       │   ├── observability.rs
│       │   ├── bin/ovaultd-admin.rs
│       │   ├── http/
│       │   │   ├── mod.rs
│       │   │   ├── auth.rs
│       │   │   ├── vaults.rs
│       │   │   ├── blobs.rs
│       │   │   └── history.rs
│       │   ├── sync/
│       │   │   ├── mod.rs
│       │   │   ├── session.rs
│       │   │   ├── commit.rs
│       │   │   └── delta.rs
│       │   ├── storage/
│       │   │   ├── mod.rs
│       │   │   ├── sqlite.rs
│       │   │   ├── filesystem.rs
│       │   │   └── notifier.rs
│       │   └── maintenance/
│       │       ├── mod.rs
│       │       ├── backup.rs
│       │       ├── gc.rs
│       │       └── rotation.rs
│       └── tests/
│           ├── auth.rs
│           ├── cas.rs
│           ├── delta.rs
│           ├── blobs.rs
│           ├── crash_commit.rs
│           ├── rotation.rs
│           └── history_gc.rs
├── packages/
│   ├── protocol/
│   │   ├── package.json
│   │   └── src/
│   │       ├── messages.ts
│   │       └── canonical.ts
│   ├── crypto/
│   │   ├── package.json
│   │   ├── src/
│   │   │   ├── CryptoProvider.ts
│   │   │   ├── sync2.ts
│   │   │   └── stream.ts
│   │   └── test/sync2.test.ts
│   ├── core/
│   │   ├── package.json
│   │   ├── src/
│   │   │   ├── SyncClient.ts
│   │   │   ├── SyncConnection.ts
│   │   │   ├── FileAdapter.ts
│   │   │   ├── StateStore.ts
│   │   │   ├── SyncFilter.ts
│   │   │   ├── outbox.ts
│   │   │   ├── apply.ts
│   │   │   ├── reconcile.ts
│   │   │   ├── retry.ts
│   │   │   ├── merge.ts
│   │   │   ├── history.ts
│   │   │   └── rotation.ts
│   │   └── test/
│   │       ├── reconcile.test.ts
│   │       ├── modes.test.ts
│   │       ├── filters.test.ts
│   │       └── merge.test.ts
│   └── cli/
│       ├── package.json
│       └── src/
│           ├── main.ts
│           ├── browser-runtime.ts
│           ├── NodeFileAdapter.ts
│           └── credentials.ts
├── plugins/ovaultd/
│   ├── package.json
│   ├── manifest.json
│   ├── versions.json
│   ├── styles.css
│   ├── esbuild.config.mjs
│   └── src/
│       ├── main.ts
│       ├── ObsidianFileAdapter.ts
│       ├── SettingsTab.ts
│       ├── ActivityView.ts
│       ├── ConflictView.ts
│       ├── HistoryView.ts
│       └── RecoveryModal.ts
├── migrations/
│   ├── 001_init.sql
│   ├── 002_protocol_support.sql
│   └── 003_epoch_support.sql
├── docker/
│   ├── Dockerfile
│   ├── compose.dev.yaml
│   └── Caddyfile.example
├── scripts/
│   ├── ci.sh
│   ├── backup.sh
│   └── restore.sh
├── tests/
│   ├── fixtures/
│   │   ├── crypto/sync2.json
│   │   ├── protocol/v2.json
│   │   └── merge/cases.json
│   ├── scripts/
│   │   ├── server-smoke.sh
│   │   └── scan-canary.sh
│   ├── browser/
│   │   ├── durability.spec.ts
│   │   └── credentials.spec.ts
│   ├── e2e/
│   │   ├── two-devices.spec.ts
│   │   ├── recovery.spec.ts
│   │   └── history.spec.ts
│   ├── harness/
│   │   ├── server.ts
│   │   ├── device.ts
│   │   └── faults.ts
│   ├── playwright.config.ts
│   └── package.json
├── xtask/
│   ├── Cargo.toml
│   └── src/main.rs
├── spikes/crypto-interop/
│   ├── run.sh
│   ├── compare.py
│   ├── README.md
│   ├── js/
│   │   ├── package.json
│   │   ├── package-lock.json
│   │   ├── spike.mjs
│   │   └── streaming.mjs
│   └── rust/
│       ├── Cargo.toml
│       ├── Cargo.lock
│       └── src/main.rs
└── docs/
    ├── build-plan.md
    ├── implementation-map.md
    ├── crypto-gate.md
    ├── runbook.md
    ├── test-matrix.md
    ├── benchmarks.md
    └── manual-desktop-checklist.md
```

Se conservan además los documentos y reportes existentes, omitidos del árbol para no repetirlos.

### Decisiones de estructura

- **Tres crates funcionales, no un crate por endpoint.** `ovaultd-protocol` concentra el contrato; `ovaultd-crypto`, verificación e interop; `ovaultd-server`, el único backend implementado.
- **`MetadataStore`, `BlobStore` y `Notifier`** se definen en `storage/mod.rs`. Sus únicas implementaciones son SQLite, filesystem y notificación en proceso.
- **El núcleo TS no importa Obsidian ni Node.** CLI y plugin comparten cripto, protocolo, estado y algoritmo.
- **IndexedDB es el único backend del estado del cliente.** Para la CLI durable se usa Chromium headless mediante Playwright, con perfil persistente y un puente restringido al `NodeFileAdapter`. No se usa `fake-indexeddb` en producción ni se agrega SQLite local.
- Ese runtime agrega tamaño y RAM a la CLI, pero evita inventar un segundo motor durable. El navegador solamente carga recursos locales; la CLI no necesita Internet después de instalarse.
- `002_protocol_support.sql` y `003_epoch_support.sql` se crean **solamente si** el esquema existente no cubre las necesidades correspondientes. No se duplican tablas de idempotencia o epochs que ya existan en `001_init.sql`.
- El spike queda excluido del workspace Cargo principal y mantiene implementaciones independientes. Compartir su implementación de serialización con producción reduciría el valor del test de interop.

---

## 1. Fases

### Fase 0.2 — Gateway cripto `sync2` y reejecución del spike (1–2 semanas)

**Objetivo:** demostrar interop y viabilidad del sobre `sync2` antes de escribir el servidor.

**Prerequisitos:** documentos y ADR existentes disponibles; ninguna dependencia del resultado viejo `sync1`.

#### Entregables

- Actualizados: `spikes/crypto-interop/run.sh`, `compare.py`, `README.md`, `js/spike.mjs`, `rust/src/main.rs` y sus lockfiles.
- Nuevos: `spikes/crypto-interop/js/streaming.mjs`, `tests/fixtures/crypto/sync2.json`.
- Evidencia: `docs/crypto-gate.md`, `docs/implementation-map.md`.

#### Pasos

1. **Auditar lo que efectivamente está congelado.**

   ```bash
   git status --short
   cat docs/crypto.md docs/protocol.md
   cat docs/decisions/000{1,2,3,4,5}-*.md
   sqlite3 :memory: '.read migrations/001_init.sql' '.schema'
   ```

   Registrar en `implementation-map.md` el mapeo de:

   - `blob_id` de protocolo a `files.blob_hash`, `revisions.blob_hash` y `blobs.hash`.
   - `t_mac`, `sig`, `epoch`, `path_enc`, `batch_id`.
   - Almacenamiento de `operation_id` y su respuesta durable.
   - Semántica exacta de `expected_rev`, `min_rev` y `next_since`.
   - Codificación binaria firmada, dominios `sync2`, normalización y límites del KDF.

2. **Cerrar dos preguntas antes de generar vectores.**

   - Cómo obtiene el cliente el `vault_id` que entra en el AAD antes de persistir `wrapped_key_pw` y `wrapped_key_rc`.
   - Cómo firma el cliente la revisión que asigna el servidor bajo CAS.

   Implementar la secuencia que ya fije v2, no una alternativa nueva.

   > ⚠️ DISCREPANCIA: si los documentos no determinan creación con AAD y firma de revisión bajo CAS, falta contrato ejecutable; requiere aclaración aprobada antes de abrir la fase 1, nunca una clave privada en el servidor.

3. **Actualizar dependencias del spike y fijarlas.**

   ```bash
   npm --prefix spikes/crypto-interop/js i --save-exact \
     @noble/hashes @noble/ciphers @noble/curves

   cargo add --manifest-path spikes/crypto-interop/rust/Cargo.toml \
     argon2 hkdf hmac sha2 chacha20poly1305 unicode-normalization \
     serde_json hex

   cargo add --manifest-path spikes/crypto-interop/rust/Cargo.toml \
     ed25519-dalek
   ```

   Usar los parámetros de `docs/crypto.md`: Argon2id de 64 MiB, tres iteraciones y paralelismo cuatro; alternativa scrypt solamente según el contrato versionado.

4. **Generar casos independientes Rust y JS.**

   Cubrir KEK, `vault_key`, ambas envolturas, HKDF por dominio, Ed25519, challenge, `path_id`, `path_enc` con AAD, contenido, `blob_id`, `t_mac` y firma de registros, incluido `del`.

   Usar RNG inyectable **solo en tests**. Los nonces aleatorios se fijan para reproducibilidad del vector; el runtime nunca reutiliza esos valores.

5. **Probar streaming real, no solamente cifrado de un buffer.**

   Comparar el sobre obtenido con diferentes particiones de entrada contra la implementación de referencia de mensaje completo. Medir memoria con un adjunto grande.

   Verificar el camino de dos pasadas, incluyendo modificación del archivo entre pasadas: debe reiniciar o rechazar, nunca publicar ciphertext inválido.

   > ⚠️ DISCREPANCIA: una API AEAD que acepta únicamente el archivo completo no cumple memoria acotada; no se reemplaza por AEAD independiente por chunk, porque cambiaría `sync2`.

   Si las versiones elegidas no permiten el adaptador incremental compatible, resolverlo y revisarlo en este gateway. No trasladar el problema al plugin.

6. **Congelar los vectores después de revisar ambos resultados.**

   Extender `run.sh` con dos modos separados:

   ```bash
   mkdir -p .tmp/sync2
   ./spikes/crypto-interop/run.sh --emit-candidates .tmp/sync2

   # Después de comparar y revisar los candidatos:
   cp .tmp/sync2/rust.json tests/fixtures/crypto/sync2.json
   ./spikes/crypto-interop/run.sh --check
   ```

   `--check` no escribe el fixture y falla ante cualquier diferencia, vector faltante o dominio `sync1`.

#### Aceptación ejecutable

| Comando | Resultado esperado / defecto que detecta |
|---|---|
| `./spikes/crypto-interop/run.sh --check` | `INTEROP sync2 OK`; falla si cambia un byte de serialización o derivación. |
| `npm --prefix spikes/crypto-interop/js test` | Rechaza password incorrecta, AAD alterado, firma inválida y ciphertext truncado. |
| `cargo test --manifest-path spikes/crypto-interop/rust/Cargo.toml --locked` | Mismos positivos y negativos en Rust. |
| `node spikes/crypto-interop/js/streaming.mjs` | Misma salida para particiones distintas; memoria no proporcional al adjunto. |

Agregar explícitamente `password_rewrap_preserves_ids`: cambiar el KEK mantiene `path_id`, `blob_id` y `t_mac`.

**Riesgos y trampas:** dos implementaciones pueden compartir el mismo error conceptual. Mitigar con revisión de los bytes firmados, vectores conocidos de las primitivas y casos negativos; no alcanza con que ambos impriman el mismo JSON.

**Commits sugeridos:**

1. `docs: mapear contrato vigente y brechas del gateway`
2. `test: actualizar spike independiente a sync2`
3. `test: congelar vectores sync2 y casos negativos`
4. `docs: registrar evidencia y habilitar fase 1`

---

### Fase 1 — Servidor MVP cifrado, auth firmada, CAS, WS y CLI mínimo (4–5 semanas)

**Objetivo:** persistir y distribuir revisiones cifradas, autenticadas y atómicas mediante el protocolo v2.

**Prerequisitos:** gateway `sync2` completamente verde, incluida la secuencia de firma y creación del vault.

#### Entregables

- Workspace raíz y `crates/ovaultd-{protocol,crypto,server}/`.
- `packages/protocol/`, `packages/crypto/`, `packages/cli/`.
- `xtask/`, `docker/Dockerfile`, `docker/compose.dev.yaml`, `compose.yaml`, `.env.example`.
- `tests/fixtures/protocol/v2.json`, `tests/scripts/server-smoke.sh`, `tests/scripts/scan-canary.sh`.
- Tests Rust `auth.rs`, `cas.rs`, `delta.rs`, `blobs.rs`.
- `migrations/002_protocol_support.sql`, únicamente si hace falta.

#### Pasos

1. **Crear los workspaces sin tocar el spike.**

   ```bash
   cargo new --lib crates/ovaultd-protocol
   cargo new --lib crates/ovaultd-crypto
   cargo new --bin crates/ovaultd-server
   cargo new --bin xtask
   ```

   Declarar esos miembros y excluir `spikes/crypto-interop/rust`. En `.cargo/config.toml`, definir:

   ```toml
   [alias]
   xtask = "run --package xtask --"
   ```

   Pinear toolchain, Node y dependencias; guardar ambos lockfiles.

   ```bash
   cargo add -p ovaultd-server axum --features ws
   cargo add -p ovaultd-server tokio --features full
   cargo add -p ovaultd-server sqlx \
     --features runtime-tokio,sqlite,migrate
   cargo add -p ovaultd-server serde --features derive
   cargo add -p ovaultd-server serde_json tracing tracing-subscriber \
     tower-http clap uuid sha2 argon2
   cargo add -p ovaultd-server --dev tempfile
   ```

   Usar SQLx con consultas tipadas y migraciones; no introducir PostgreSQL como dependencia accidental.

2. **Crear los paquetes npm y portar el contrato verificado.**

   Nombres de workspace: `@ovaultd/protocol`, `@ovaultd/crypto`, `@ovaultd/core`, `@ovaultd/cli`, `@ovaultd/plugin` y `@ovaultd/tests`.

   ```bash
   npm i -D --save-exact typescript vitest eslint \
     typescript-eslint esbuild @playwright/test @types/node

   npm i -w @ovaultd/crypto --save-exact \
     @noble/hashes @noble/ciphers @noble/curves
   npm i -w @ovaultd/cli --save-exact commander
   ```

   `CryptoProvider` cifra desde su primera implementación. `ovaultd-server` usa únicamente la API de verificación pública; jamás recibe `vault_key`, KEK ni recovery en claro.

3. **Levantar SQLite y auth antes de aceptar blobs.**

   ```bash
   cargo install sqlx-cli --locked \
     --no-default-features --features sqlite

   mkdir -p .tmp/server
   export DATABASE_URL="sqlite://$PWD/.tmp/server/metadata.db"
   sqlx database create
   sqlx migrate run --source migrations
   ```

   Configurar WAL, foreign keys, `busy_timeout` y sincronización durable. Agregar:

   - Bootstrap local de `accounts`, sin registro público improvisado.
   - Login y MFA opcional; `devices.token_hash`, revocación y auditoría.
   - Challenge aleatorio, vencimiento, uso único y vinculación a sesión/vault/dispositivo.
   - Verificación con `vaults.vault_pubkey`.
   - ACL de propietario en control, WS y blobs; sin UI de sharing.

4. **Implementar publicación de blobs y commit como una única ruta de escritura.**

   En `storage/filesystem.rs`: temporal → validación de longitud/hash → `fsync` → publicación atómica → sincronización del directorio.

   Después, en `sync/commit.rs`, una transacción:

   - Verifica autorización vigente y `operation_id`.
   - Devuelve el ACK persistido si la operación ya se confirmó.
   - Rechaza reutilización del mismo ID con otro contenido.
   - Comprueba `expected_rev` contra el valor definido por v2.
   - Asigna revisión; actualiza `files`, inserta `revisions` y ajusta `blobs.refs`.
   - Persiste la respuesta idempotente.
   - Confirma antes de emitir ACK o notificación.

   Un `batch_id` produce una revisión con todas sus filas. `rename + edit` no se parte en commits independientes.

5. **Implementar WS, delta y transferencias.**

   `session.rs` maneja estados de handshake y una cola de recepción binaria. `delta.rs` pagina por revisiones completas y devuelve `next_since`/`min_rev`.

   El notifier es una señal de “hay revisiones”; ante lag del canal se relee el oplog. No se usa un broadcast descartable como fuente de verdad.

   Elegir transporte una vez por blob: WS inline hasta el umbral; HTTP reanudable por encima. Persistir offsets confirmados, validar `Range`/`Content-Range` y no confiar en la longitud declarada.

   Aplicar cuotas opt-in antes y durante la recepción, con reservas que impidan que dos uploads simultáneos excedan el límite.

6. **Entregar CLI mínimo y Compose.**

   CLI: `login`, `logout`, `list-remote`, `create-remote`, `setup`, `status`. Crear vault ya genera y exige guardar recovery; la fase 4 mejora el ciclo de vida, no introduce la primera recovery.

   `ovaultd-admin`: creación de cuenta, comprobación de salud e inspección operacional sin descifrar contenido.

   ```bash
   cp .env.example .env
   chmod 600 .env

   docker compose -f compose.yaml -f docker/compose.dev.yaml \
     up -d --build
   curl --fail http://127.0.0.1:8080/healthz
   ```

   La variante de desarrollo admite una imagen local. La distribución soportada usa `repositorio@sha256:...`, obtenido de una imagen publicada real, nunca un digest ficticio. Producir y probar imágenes `amd64` y `arm64` antes de entregar la fase.

   Compose incluye usuario non-root, volúmenes nombrados, rootfs read-only, `cap_drop: ALL`, `no-new-privileges`, límites y healthcheck. Logs JSON, `/metrics` y auditoría excluyen secretos y payloads.

#### Aceptación ejecutable

```bash
cargo test -p ovaultd-server --test auth
cargo test -p ovaultd-server --test cas
cargo test -p ovaultd-server --test delta
cargo test -p ovaultd-server --test blobs
bash tests/scripts/server-smoke.sh
bash tests/scripts/scan-canary.sh
```

Tests obligatorios:

- `concurrent_expected_rev_has_one_winner`: dos escritores sobre la misma base; uno confirma, otro recibe conflicto.
- `retry_after_lost_ack_returns_same_revision`: un reintento no crea revisión.
- `restore_deleted_same_blob_is_not_noop`: mismo contenido después de borrar sí cambia el estado.
- `delta_never_splits_batch`: `limit=1` no corta una revisión de varias filas.
- `challenge_replay_is_rejected` y `revoked_device_cannot_download`.
- `cross_vault_blob_read_is_denied`, aunque el solicitante conozca el hash.
- `no_plaintext_in_server_storage`: canarias ausentes en SQLite, WAL, CAS, temporales y logs.

`server-smoke.sh` usa `curl` y cuerpos firmados por el cliente de prueba: crea un vault, confirma dos revisiones y comprueba la delta exacta. No usa firmas constantes ni credenciales de producción.

**Riesgos y trampas:** ACK antes de commit, dedupe como falsa idempotencia y serialización Rust/TS divergente. Mitigar centralizando el commit y ejecutando fixtures comunes desde ambos lenguajes.

**Commits sugeridos:** workspace; contrato/cripto; auth; CAS filesystem; commit transaccional; WS/delta; uploads/cuotas; CLI; Compose/tests.

---

### Fase 2 — Núcleo durable offline, backup/restore y pruebas de fallos (4–6 semanas)

**Objetivo:** conservar cambios locales y converger después de reinicios, cortes de red o pérdida del ACK.

**Prerequisitos:** servidor y pruebas de invariantes verdes; Compose funcional.

#### Entregables

- `packages/core/src/{StateStore,outbox,apply,reconcile,retry,SyncConnection}.ts`.
- `packages/cli/src/{browser-runtime,NodeFileAdapter,credentials}.ts`.
- `tests/browser/durability.spec.ts`, `credentials.spec.ts`.
- `crates/ovaultd-server/tests/crash_commit.rs`.
- `scripts/backup.sh`, `scripts/restore.sh`, `docs/runbook.md`.

#### Pasos

1. **Construir estado durable y runtime CLI.**

   ```bash
   npm i -w @ovaultd/core --save-exact idb
   npm i -w @ovaultd/cli --save-exact playwright
   npx playwright install chromium
   ```

   `StateStore.ts` mantiene stores IndexedDB versionados para índice, base sincronizada, outbox, transferencias parciales, intenciones de aplicación y checkpoint.

   El perfil Chromium de cada dispositivo tiene directorio privado y origen local estable. El puente de archivos restringe operaciones al vault, no sigue symlinks y no expone un servidor de archivos a la red.

   La variable de entorno de token tiene prioridad y no se copia al disco. Tokens persistidos: permisos `0600`. Password, `vault_key` y KEK permanecen en memoria.

2. **Persistir lo local antes de aplicar lo remoto.**

   Para cada cambio, capturar una versión estable, `content_hash`, base y contenido recuperable antes de modificar el archivo visible. Una outbox que guarda solo el nombre del archivo no alcanza: el archivo puede cambiar o desaparecer.

   El `operation_id` se genera una vez y se conserva junto al payload firmado. Reintento de idéntico payload: mismo ID. Rebase con contenido o precondición nuevos: nueva operación.

3. **Resolver atomicidad IndexedDB/filesystem con journal.**

   `apply.ts` persiste una intención, descarga a temporal, verifica firmas/hash/AEAD, publica el archivo y completa índice/checkpoint.

   Al reiniciar, compara estado previo y resultado esperado. Si aparece una tercera versión local, la preserva y abre resolución; no la pisa.

   Avanzar `since` y enviar la confirmación de aplicación únicamente cuando la revisión completa quedó recuperablemente aplicada. Recibir un frame no equivale a aplicarlo.

4. **Completar el ciclo offline y CLI.**

   Implementar todos los comandos restantes: `list-local`, `sync`, `sync --continuous`, `config`, `unlink`.

   Agregar hash-diff, tombstones con retiro efectivo al trash, renames por `prev_path_id`, fencing, cola serial y backoff `2^n × 5 s`, cap cinco minutos. Reconexión con jitter; polling de rescate a 30 segundos.

   Si `since` quedó fuera de `min_rev`, preservar la outbox y hacer bootstrap coherente. No borrar el estado local para “arreglar” el cursor.

5. **Respaldar antes de hacer sync sobre datos reales.**

   Elegir la alternativa simple permitida: backup con el servidor detenido. `backup.sh` detiene, espera cierre, toma SQLite mediante backup consistente y archiva CAS; finalmente reinicia incluso ante error.

   ```bash
   bash scripts/backup.sh --output .tmp/backup.tar.zst
   bash scripts/restore.sh \
     --input .tmp/backup.tar.zst \
     --project ovaultd-restore-check
   ```

   `restore.sh` rechaza destinos no vacíos salvo confirmación explícita, verifica checksums, restaura en volúmenes separados y comprueba integridad SQL y referencias CAS.

6. **Agregar inyección de fallos y validar capacidades de I/O.**

   Failpoints de servidor exclusivamente en builds de test: después de temporal, `fsync`, publicación CAS, commit SQL y antes de ACK.

   Cliente: terminar Chromium después de guardar outbox, recibir remoto, publicar archivo y antes de confirmar IndexedDB.

   Verificar temprano las capacidades que deberá proporcionar `ObsidianFileAdapter`: streaming, no-follow y publicación recuperable.

   > ⚠️ DISCREPANCIA: si las APIs públicas de Obsidian no permiten streaming o distinguir symlinks sin Node, esos requisitos necesitan una decisión explícita; no se declaran cumplidos usando `readBinary()` de archivo completo o siguiendo enlaces.

#### Aceptación ejecutable

```bash
cargo test -p ovaultd-server --test crash_commit
npm run test:browser -- tests/browser/durability.spec.ts
npm run test:browser -- tests/browser/credentials.spec.ts
cargo xtask chaos --seed 42 --operations 1000
cargo xtask backup-restore
```

Esperado:

- Ninguna revisión confirmada referencia un blob inexistente.
- Un blob publicado sin commit queda huérfano recuperable por GC, no como archivo visible.
- La outbox sobrevive al cierre abrupto.
- Un ACK perdido no duplica revisiones.
- Restore en volúmenes nuevos recupera árbol, historial, wrappers y firmas.
- `remote_apply_preserves_unqueued_local_edit` falla si se invierte el orden local/remoto.

**Riesgos y trampas:** `fake-indexeddb` oculta fallos reales; matar una promesa no simula un proceso muerto. Ejecutar los casos críticos contra Chromium y procesos reales. Los failpoints no reemplazan pruebas en VM para garantías de pérdida de energía.

**Commits sugeridos:** IndexedDB; runtime CLI; outbox; journal de aplicación; reconexión; backup/restore; caos reproducible.

---

### Fase 3 — Plugin Obsidian, motor de sync, merge 3 vías y pull-only (5–6 semanas)

**Objetivo:** sincronizar dos vaults desktop en tiempo real sin perder ediciones concurrentes.

**Prerequisitos:** núcleo durable y restore verificados; capacidades reales del adaptador resueltas.

#### Entregables

- `plugins/ovaultd/manifest.json`, `versions.json`, `package.json`, `esbuild.config.mjs`.
- `plugins/ovaultd/src/{main,ObsidianFileAdapter,SettingsTab,ActivityView,ConflictView}.ts`.
- `packages/core/src/{SyncClient,SyncFilter,merge}.ts`.
- `packages/core/test/{reconcile,modes,filters,merge}.test.ts`.
- `tests/e2e/two-devices.spec.ts`, `docs/manual-desktop-checklist.md`, `docs/benchmarks.md`.

#### Pasos

1. **Construir el plugin mínimo sobre el núcleo, no otro motor.**

   ```bash
   npm i -D -w @ovaultd/plugin --save-exact obsidian esbuild
   npm i -w @ovaultd/core --save-exact diff-match-patch
   npm i -D --save-exact @types/diff-match-patch
   npm run build -w @ovaultd/plugin
   ```

   Manifest: `id: ovaultd`, nombre `oVaultd Plugin`, `isDesktopOnly: true`. Elegir `minAppVersion` según la versión realmente probada.

   El bundle deja `obsidian` externo y falla si importa módulos de Node desde plugin/core/crypto.

2. **Conectar eventos y reconciliación.**

   `ObsidianFileAdapter` traduce `create`, `modify`, `delete`, `rename`, startup y resume. Coalescer eventos sin perder el último cambio.

   Evitar loops de watcher mediante journal y hash esperado, no ignorando todos los eventos durante un intervalo.

   Validar paths descifrados antes de escribir: traversal, rutas absolutas, nombres inválidos, colisiones de case/normalización y symlinks. Mostrar “salteado con motivo”.

3. **Implementar la tabla de decisión contra la base.**

   En `SyncClient.ts`:

   - Local distinto, remoto igual a base: push.
   - Remoto distinto, local igual a base: pull.
   - Ambos iguales entre sí: convergencia.
   - Ambos distintos: resolución con base común o copia.

   Ordenar carpetas primero y archivos por tamaño. No tomar `mtime` como autoridad de contenido.

4. **Implementar merge conservador.**

   `merge.ts` calcula diferencias base→ours y base→theirs, proyecta cambios a coordenadas de la base y detecta solapamientos antes de aplicar parches.

   Inserciones distintas en el mismo punto son conflicto. Ningún parche parcialmente aplicado se considera éxito. Configurar explícitamente el umbral de matching.

   `.md`: merge char-level. Binarios, `.canvas`, `.base` o falta de base: copia timestamp y remoto en el original. La resolución genera una revisión nueva.

5. **Completar modos, filtros y guardas.**

   `pull-only` nunca publica operaciones, tampoco copias locales de conflicto. `mirror-remote` mantiene su semántica diferenciada según contrato, con protección local antes de descartar cambios.

   Probar exclusiones por carpeta, tipos, dotfiles, `workspace*.json`, `node_modules`, File recovery y configuración del propio plugin. Excluir no significa borrar el remoto.

   Cliente nuevo/vacío o archivos ausentes sin evidencia confiable de borrado: bloquear propagación y pedir confirmación. Fallos de lectura o placeholders tampoco son tombstones.

6. **Agregar UI y operar dos dispositivos.**

   Status bar, pause/resume, desconectado, log persistente acotado, settings por dispositivo y revisión de conflictos. `unlink` purga IndexedDB y transferencias parciales, no los archivos del vault.

   Instalar el build en dos vaults de prueba:

   ```bash
   npm run install:dev -w @ovaultd/plugin -- \
     --vault "$HOME/tmp/ovaultd-device-a"
   npm run install:dev -w @ovaultd/plugin -- \
     --vault "$HOME/tmp/ovaultd-device-b"

   cargo xtask e2e --case two-devices
   cargo xtask bench --notes 1000 --attachments-mib 100
   ```

#### Aceptación ejecutable

```bash
npm test -- packages/core/test/reconcile.test.ts
npm test -- packages/core/test/modes.test.ts
npm test -- packages/core/test/filters.test.ts
npm test -- packages/core/test/merge.test.ts
cargo xtask e2e --case two-devices
```

Tests que deben fallar ante regresión:

- `pull_only_emits_zero_pushes`.
- `fresh_empty_client_cannot_delete_remote`.
- `excluded_folder_does_not_become_tombstones`.
- `same_paragraph_disjoint_edits_merge`.
- `same_position_insertions_conflict`.
- `incoming_binary_before_handler_is_queued`.

Benchmark LAN reproducible: 1.000 notas + 100 MB, inicial menor a 30 segundos y delta menor a un segundo. Registrar CPU, disco, red, versiones, repetición y pico de RAM. Excluir interacción humana del cronómetro y publicar aparte el costo del KDF.

Los tests headless prueban el núcleo; **no reemplazan** el checklist sobre Obsidian real en dos dispositivos.

**Riesgos y trampas:** la integración de watcher suele duplicar operaciones; el fuzzy matching puede inventar un merge aceptable pero incorrecto. Mitigar con journal, proyección sobre base y fallback conservador.

**Commits sugeridos:** shell del plugin; adaptador; motor; merge; modos/filtros; UI; E2E y benchmark.

---

### Fase 4 — E2E completo, rotación de claves y recovery (3–4 semanas)

**Objetivo:** cerrar el ciclo de vida de claves sin rehacer el cifrado ya usado desde la fase 1.

**Prerequisitos:** sincronización real y durabilidad verdes; fixtures `sync2` sin cambios incompatibles.

#### Entregables

- `packages/core/src/rotation.ts`.
- `plugins/ovaultd/src/RecoveryModal.ts`.
- `crates/ovaultd-server/src/maintenance/rotation.rs`.
- `crates/ovaultd-server/tests/rotation.rs`.
- `tests/e2e/recovery.spec.ts`.
- `migrations/003_epoch_support.sql`, si el contrato requiere soporte aún ausente.
- Procedimientos de claves en `docs/runbook.md`.

#### Pasos

1. **Auditar secretos y onboarding.**

   Confirmar que password de cuenta y password E2E son conceptos independientes. El servidor persiste `wrapped_key_pw`, `wrapped_key_rc`, `vault_pubkey`, `epoch`, salt y parámetros; nunca la clave privada.

   Mostrar recovery con checksum, exigir confirmación y ofrecer exportación explícita. No guardarla en settings sincronizados, logs o CAS.

2. **Implementar cambio de password como reenvoltura.**

   Derivar nuevo KEK con salt y parámetros versionados; desenvolver y volver a envolver la misma `vault_key`. Actualizar metadata con precondición para evitar carreras.

   Regenerar recovery según contrato y revocar dispositivos afectados. No recalcular `path_id`, `blob_id` ni `t_mac`.

3. **Implementar recuperación en un dispositivo limpio.**

   El tercer dispositivo usa solo recovery y metadata remota para abrir el vault; verifica `vault_pubkey` y todas las firmas recibidas. Después permite establecer password nueva mediante el procedimiento autorizado.

   ```bash
   npm run cli -- setup --recovery
   cargo xtask e2e --case recovery
   ```

   Los secretos se leen por prompt/stdin, no como argumentos visibles en historial o lista de procesos.

4. **Implementar rotación por compromiso como trabajo durable.**

   Crear nuevo `epoch` y nueva `vault_key`; re-cifrar cuerpos y migrar también identidades de paths, bindings y material público conforme al contrato. Checkpoint por lote y reanudación después de crash.

   Nunca modificar silenciosamente registros firmados inmutables. Antes de implementar, verificar cómo el contrato conserva verificación y lectura de historia de epochs anteriores y cómo protege snapshots.

   > ⚠️ DISCREPANCIA: si `sync2` no define transición de identidad, claves públicas históricas y tratamiento de revisiones/snapshots al rotar `vault_key`, la rotación por compromiso queda bloqueada hasta documentarlo; incrementar `epoch` solamente no alcanza.

   No habilitar GC de material anterior hasta completar la transición y verificar sus referencias.

5. **Endurecer transporte y ataques al servidor ciego.**

   Probar CA propia, host devuelto por `access`, rechazo de cambio de origen no autorizado y ausencia de downgrade TLS. Documentar instalación LAN con `docker/Caddyfile.example`.

   Verificar límites de entrada del KDF para que metadata maliciosa no dispare un consumo arbitrario de memoria.

#### Aceptación ejecutable

```bash
./spikes/crypto-interop/run.sh --check
cargo test -p ovaultd-server --test rotation
cargo xtask e2e --case recovery
npm run test:browser -- tests/browser/credentials.spec.ts
bash tests/scripts/scan-canary.sh
```

Esperado:

- Password incorrecta no desenvuelve la clave.
- Recovery válida recupera un dispositivo sin estado previo.
- `password_rotation_does_not_reupload` mantiene hashes, bindings y cantidad de blobs.
- `rotation_crash_resumes_without_mixed_head` no expone un árbol parcialmente migrado.
- `swapped_path_enc_is_rejected` y `tampered_tombstone_is_rejected`.
- Un dispositivo con checkpoint rechaza una historia que retrocede.

> ⚠️ DISCREPANCIA: firmas y checkpoint local detectan retrocesos observables, pero no prueban completitud frente a un servidor que oculta datos ni evitan todos los forks para un dispositivo nuevo; no se documenta una garantía superior.

**Riesgos y trampas:** prometer revocación retroactiva. Un atacante que guardó recovery, wrapper y ciphertext anteriores puede seguir leyéndolos. La rotación protege el material nuevo; no deshace filtraciones.

**Commits sugeridos:** onboarding; rewrap; recovery; transición de epoch; fallos de rotación; pruebas adversariales.

---

### Fase 5 — Historia, restore, preview y conflictos finos (3–5 semanas)

**Objetivo:** recuperar cualquier versión retenida y resolver conflictos complejos sin reescribir historia.

**Prerequisitos:** rotación/recovery verificadas y semántica de epochs históricos cerrada.

#### Entregables

- `crates/ovaultd-server/src/http/history.rs`.
- `crates/ovaultd-server/src/maintenance/gc.rs`.
- `packages/core/src/history.ts`.
- `plugins/ovaultd/src/HistoryView.ts`.
- `tests/fixtures/merge/cases.json`.
- `crates/ovaultd-server/tests/history_gc.rs`.
- `tests/e2e/history.spec.ts`.

#### Pasos

1. **Exponer historia y snapshots sin descifrar.**

   Listar `revisions` con dispositivo, `path_enc`, `blob_hash`, `t_mac`, `sig` y `epoch`. Resolver renames mediante metadata histórica, no únicamente mirando `files`.

   Crear snapshots etiquetados con corte coherente en `rev`; incrementar referencias de sus manifests en la misma transacción.

2. **Implementar preview cliente y CLI.**

   Descargar la versión seleccionada, verificarla y descifrarla localmente. Mostrar contenido/diff sin escribir en el vault.

   ```bash
   npm run cli -- history "Notas/Proyecto.md"
   npm run cli -- preview "Notas/Proyecto.md" --rev 12
   ```

   Historial y preview deben funcionar también para archivos borrados y nombres anteriores a un rename.

3. **Restaurar mediante nuevas operaciones firmadas.**

   El cliente prepara y firma el restore con `expected_rev` y `operation_id`; el servidor no puede generar firmas del vault.

   Restore individual y masivo usan el flujo canónico de `/restore`; un restore masivo comparte `batch_id` y revisión. Las filas nuevas emplean `add`/`mod`/`del`/`move` según corresponda, no un valor `restore` incompatible con el `CHECK`.

   ```bash
   npm run cli -- restore "Notas/Proyecto.md" --rev 12
   npm run cli -- snapshots create --label "antes de reorganizar"
   ```

4. **Completar UI y corpus de conflictos.**

   Agregar papelera navegable, selección múltiple, preview antes de restaurar y contexto del dispositivo.

   Ampliar corpus con frontmatter, tablas, listas, code fences, Unicode, párrafos largos, delete/edit, rename/edit y rename/rename. Conservar ambas versiones cuando el resultado no sea seguro.

   Los markers visibles se escapan/renderizan de forma segura; el contenido de una nota no se transforma en HTML ejecutable dentro de la UI.

5. **Implementar retención y GC seguros.**

   Recalcular el conteo esperado desde:

   - `files` no borrados.
   - Revisiones retenidas.
   - Referencias de `snapshots.manifest`.

   `gc --dry-run` es el default. `gc --verify` y borrado destructivo corren con exclusión de escrituras, incluidos uploads. Aplicar gracia a blobs nuevos y temporales.

   ```bash
   docker compose stop app
   docker compose run --rm app ovaultd-admin gc --verify
   docker compose run --rm app ovaultd-admin gc --dry-run
   docker compose up -d
   ```

   La compaction conserva revisiones completas y actualiza la frontera de catch-up según v2; no deja huecos que se presenten como una delta íntegra.

   Retención diferenciada por tipo solamente mediante el mecanismo ya definido por el contrato, no inspeccionando nombres cifrados.

   > ⚠️ DISCREPANCIA: si no existe información autorizada para distinguir notas de adjuntos, RF-31 requiere aclaración; el servidor ciego no puede inferir extensiones desde `path_enc`.

6. **Cerrar el MVP con restore operacional y benchmark completo.**

   ```bash
   cargo xtask e2e --case history
   cargo xtask backup-restore
   cargo xtask bench --notes 10000
   bash scripts/ci.sh
   ```

#### Aceptación ejecutable

```bash
cargo test -p ovaultd-server --test history_gc
npm test -- packages/core/test/merge.test.ts
cargo xtask e2e --case history
```

Tests críticos:

- `snapshot_only_reference_survives_gc`.
- `restore_creates_new_signed_revision`.
- `bulk_restore_is_one_complete_revision`.
- `preview_does_not_mutate_vault`.
- `expired_cursor_preserves_local_outbox`.
- `historical_epoch_remains_verifiable`.
- `rename_edit_conflict_preserves_both_versions`.

**Riesgos y trampas:** optimizar GC antes de probarlo, borrar bases necesarias para merge y renderizar versiones inseguras. Mitigar con dry-run, corpus de referencias, fallback sin base y UI de contenido no ejecutable.

**Commits sugeridos:** API historia; preview; restore; snapshots/papelera; corpus fino; retención/GC; cierre de documentación.

---

## 2. Ruta crítica y dependencias

```text
0.2: contrato ejecutable + vectores sync2
  └─ 1: publicación CAS + commit + protocolo autenticado
       └─ 2: outbox + aplicación durable + restore verificado
            └─ 3: adaptador Obsidian + motor + merge conservador
                 └─ 4: ciclo de claves y epochs
                      └─ 5: historia/restore/GC final
```

La ruta crítica no pasa por la UI: pasa por **bytes canónicos → transacciones → estado durable → aplicación recuperable**.

Se puede solapar trabajo, sin adelantar aceptación:

| Mientras se construye | Se puede preparar |
|---|---|
| Gateway cripto | Inventario, checklist de schema y fixtures de protocolo. |
| Commit del servidor | Dockerfile, Compose, bootstrap y observabilidad. |
| Núcleo durable | Shell del plugin, estilos, corpus de merge. |
| Adaptador/plugin | UI de recovery e historia con fixtures. |
| Rotación | Consultas de historia y verificación derivada de refs. |

No se puede:

- Escribir servidor de producción antes de cerrar `sync2`.
- Aplicar remoto antes de disponer de outbox durable.
- Habilitar borrados reales antes de backup/restore y guardas.
- Habilitar GC destructivo antes de contar snapshots y epochs.
- Declarar el plugin usable porque pasó un test headless sin Obsidian real.

Para un solo desarrollador, “paralelizar” significa alternar tareas independientes durante esperas, no mantener cuatro ramas grandes abiertas.

---

## 3. Estrategia de pruebas

### Matriz mínima

| Nivel | Archivos | Ejecución |
|---|---|---|
| Interop independiente | `spikes/crypto-interop/*` | `./spikes/crypto-interop/run.sh --check` |
| Cripto de producción | `crates/ovaultd-crypto/tests/sync2_vectors.rs`, `packages/crypto/test/sync2.test.ts` | `cargo test -p ovaultd-crypto`; `npm test -- packages/crypto/test/sync2.test.ts` |
| Conformidad v2 | `tests/fixtures/protocol/v2.json`, tests Rust/TS | `cargo xtask protocol-check` |
| Transacciones | `crates/ovaultd-server/tests/cas.rs`, `delta.rs` | `cargo test -p ovaultd-server` |
| Cliente durable | `tests/browser/durability.spec.ts` | `npm run test:browser -- tests/browser/durability.spec.ts` |
| Crash servidor | `crates/ovaultd-server/tests/crash_commit.rs` | `cargo test -p ovaultd-server --test crash_commit` |
| Dos dispositivos | `tests/e2e/two-devices.spec.ts` | `cargo xtask e2e --case two-devices` |
| Recuperación operacional | scripts + harness | `cargo xtask backup-restore` |
| Integración Obsidian | `docs/manual-desktop-checklist.md` | Ejecución manual registrada por versión. |

`xtask` debe crear proyectos Compose y directorios temporales aislados, esperar salud, ejecutar el escenario y limpiar con `finally`/equivalente. Ante fallo conserva logs y semilla, nunca secretos.

### Vectores dorados

Cada fixture incluye entradas y resultados intermedios suficientes para localizar errores: normalización, KEK, subclaves, serialización firmada y bytes finales.

Casos mínimos: Unicode descompuesto, contenido vacío, paths distintos con igual contenido, rename sin resubida, borrado firmado, password nueva con datos estables y recuperación. Los negativos cambian un campo por vez.

La CI compara contra el archivo versionado. Nunca actualiza automáticamente el fixture.

### Conformidad de protocolo

Ejecutar el mismo corpus contra parsers Rust y TS: mensajes válidos, campos obligatorios ausentes, tamaños fuera de rango, versiones incompatibles, frames fuera de orden y operaciones repetidas.

Comprobar especialmente que el cursor avanza solamente al final de una revisión completa y que un mismo `operation_id` no acepta dos intenciones distintas.

### Escenario E2E de dos dispositivos

1. Crear A y B con perfiles IndexedDB y carpetas independientes.
2. Sincronizar una base común con notas y adjuntos.
3. Desconectar ambos; editar regiones distintas del mismo párrafo.
4. Reconectar A y confirmar su revisión.
5. Reconectar B; recibir conflicto CAS, obtener base y resolver.
6. Comparar contenido y checkpoint de A, B y servidor.
7. Repetir con inserciones en el mismo punto, binario, delete/edit y rename/edit.
8. Cortar red después del commit pero antes del ACK.
9. Reiniciar cliente y servidor; verificar ausencia de revisiones duplicadas.
10. Restaurar una versión anterior y volver a converger.

No alcanza con comparar el archivo final: comprobar historial de ambas contribuciones, ausencia de outbox pendiente y cantidad esperada de revisiones.

---

## 4. CI mínimo

El repositorio todavía no tiene remoto. Primero se crea `scripts/ci.sh`; después `.github/workflows/ci.yml` ejecuta exactamente ese script.

### Scripts npm

Definir en el `package.json` raíz:

| Script | Comando |
|---|---|
| `lint` | `eslint packages plugins tests` |
| `typecheck` | `tsc -b` |
| `test` | `vitest run` |
| `test:browser` | `playwright test --config tests/playwright.config.ts` |
| `build` | `npm run build --workspaces --if-present` |
| `cli` | `node packages/cli/dist/main.js` |

No usar `--passWithNoTests`. Los proyectos TS deben estar enumerados en las referencias de `tsc`.

### Pipeline por push

Sobre runner Linux con Rust fijado, Node fijado, Python 3, SQLite y Docker Compose:

```bash
set -euo pipefail

export CARGO_TARGET_DIR="${RUNNER_TEMP:-/tmp}/ovaultd-target"

npm ci
npm --prefix spikes/crypto-interop/js ci
npx playwright install --with-deps chromium

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked

npm run lint
npm run typecheck
npm test
npm run build

./spikes/crypto-interop/run.sh --check
cargo xtask protocol-check

npm run test:browser
cargo xtask e2e --case two-devices
cargo xtask backup-restore

git diff --exit-code -- \
  tests/fixtures/crypto/sync2.json \
  tests/fixtures/protocol/v2.json
```

Agregar cada gate cuando su fase lo implementa; no dejar un comando vacío que imprima éxito.

El workflow usa permisos mínimos `contents: read`, acciones fijadas por commit verificado y timeout. Publicación de imágenes va en un workflow de release separado: multi-arch, digest real y sin credenciales disponibles para tests de pull requests.

Para habilitar GitHub cuando se decida publicar el repositorio:

```bash
gh auth login
gh repo create ovaultd \
  --private --source=. --remote=origin --push
```

Esto es infraestructura de desarrollo, no dependencia del producto. El sync sigue funcionando sin GitHub ni Internet.

---

## 5. Riesgos de ejecución

Ordenados por probabilidad × impacto; escala de 1 a 5.

| Riesgo | P × I | Mitigación concreta |
|---|---:|---|
| Pérdida de cambios por coordinación filesystem/IndexedDB | 5 × 5 | Outbox antes del remoto, journal de aplicación y terminación real del cliente en cada frontera. |
| Contrato incompleto de firma/revisión o rotación | 4 × 5 | Gateway y revisión de epochs; bloquear la función, no inventar mensajes o firmas server-side. |
| Merge incorrecto pero aparentemente exitoso | 4 × 5 | Proyección sobre base, corpus y rechazo de éxito parcial; fallback a copia. |
| Streaming/no-follow incompatibles con API del plugin | 4 × 4 | Probar capacidades antes de integrar UI; registrar discrepancia y resolver formalmente. |
| GC elimina material histórico o de snapshots | 3 × 5 | Conteo derivado de tres fuentes, gracia, exclusión y restore posterior al GC. |
| Scope creep en producto y plataformas | 5 × 3 | Lista explícita de exclusiones; no ampliar fases sin mover fecha. |
| CLI headless consume más recursos de lo previsto | 4 × 3 | Medir temprano; un solo navegador por proceso, perfiles separados y cierre limpio. |
| Bug de interop/canonicalización | 3 × 4 | Spike independiente, fixtures inmutables y mutaciones negativas. |
| Cuotas superadas por uploads concurrentes | 3 × 4 | Reservas y límites durante streaming; liberar reservas al expirar transferencias. |
| Backup aparentemente válido pero no restaurable | 3 × 4 | Restore automático sobre volúmenes vacíos, integridad SQL y lectura de blobs referenciados. |
| Latencia inicial por KDF o hashing | 3 × 3 | KDF una vez por sesión, benchmark separado y procesamiento incremental. |
| Rollback operacional después de migración destructiva | 2 × 5 | Migraciones hacia adelante; rollback de imagen más restore probado. |

Además:

- Mantener `CARGO_TARGET_DIR`, perfiles de prueba y datos del servidor fuera del árbol del repo.
- No usar un vault personal como fixture.
- No registrar paths descifrados, recovery, tokens ni contenido en logs del servidor.
- No prometer garantías de pérdida de energía a partir de tests que solamente matan procesos.

---

## 6. Qué NO construir

- **PostgreSQL, S3, Redis, NATS, HA o multi-región:** solo nivel A y un nodo.
- **Protocolo compatible con Obsidian Sync:** se implementa v2 propio, sin ingeniería inversa.
- **Modo plaintext, passthrough o cifrado gestionado por servidor:** contradicen el alcance ciego del MVP.
- **Móvil:** `isDesktopOnly: true`; compartir núcleo no equivale a soportar iOS/Android.
- **Sharing, invitaciones, presencia, Yjs o CRDT:** quedan para la fase correspondiente; la autorización por vault sí existe desde el inicio.
- **API de lectura, búsqueda server-side o publicación de sitios:** fuera del camino crítico y del MVP.
- **Delta por bloques, AEAD por chunk con manifiestos o dedupe entre vaults:** se mantiene `sync2`, archivo completo y dedupe intra-vault.
- **Segundo backend de estado local:** no SQLite desktop paralelo a IndexedDB.
- **Config sync completo por categorías y reload de settings:** fase 6; las exclusiones de seguridad y settings per-device se implementan ahora.
- **Publicación en el directorio, auto-update o telemetría:** publicación posterior; auto-update y telemetría no se construyen.

## Empezar mañana

1. **Leer y mapear el contrato real.** Ejecutá la inspección de documentos y `.schema`, y creá `docs/implementation-map.md` con la secuencia de creación del vault, firma de revisión y persistencia de `operation_id`. No cambies `001_init.sql`.

2. **Actualizar el spike a `sync2`.** Implementá primero KEK, `vault_key` fija de test, envolturas y Ed25519 en Rust y JS. Sacá de la aceptación los resultados viejos `sync1`; no los reutilices como evidencia.

3. **Congelar y ejecutar el gateway.** Agregá binding, paths, registros, negativos y streaming; generá `tests/fixtures/crypto/sync2.json`, revisalo y corré `./spikes/crypto-interop/run.sh --check`. Recién con ese resultado verde, creá el workspace del servidor.

---
_generado: gpt-6-astra - effort=high - 339s - tokens: in=29293 out=18498 (reasoning=5696)_


---

## 7. Insumos del estudio de viabilidad del cliente

> **Provenance:** ingeniería inversa **estática** del cliente **1.14.4 (Electron 43)** (`app.asar` 74 KB +
> `obsidian.asar` 27,4 MB, 368 archivos analizados). Se midió comportamiento, tamaños y dependencias: **no se
> transportó código** (el paquete declara `UNLICENSED`/`private`).
>
> **Lo que el estudio confirma de este plan:** el host de plugins es **10/10** de dificultad (los plugins usan
> `require('obsidian')`, Node —`fs`/`path`/`child_process`— y DOM), los temas son **variables CSS**, y el Sync oficial
> es **propietario**. Por eso este proyecto es **complemento** (motor + sync propios) y no un clon: el clon completo
> queda fuera de alcance y así está decidido en §6.

### 7.1 Restricciones ya conocidas (hay que respetarlas desde el día uno, no descubrirlas en Fase 3)

- **El shell del plugin no puede depender de Node.** Nada de `child_process` ni módulos de Node: los archivos se
  acceden por `vault.adapter` y la API del plugin. El **mismo código corre en Android/iOS**, donde Node no existe.
- **Orígenes reales del cliente:** `app://obsidian.md` (escritorio) y `capacitor://localhost` (móvil). El handshake
  **WebSocket no tiene el atajo de `requestUrl`** y pasa por CORS: el servidor debe aceptar **exactamente** esos
  orígenes — nunca `*`.
- **Los temas son CSS.** Toda vista del plugin vive en el DOM y hereda las variables de tema. No inventar widgets
  nativos ni asumir control del chrome de la app.
- **Clean-room.** Del asar se lee comportamiento; **nunca** se copian fragmentos de código, strings ni assets al repo.

### 7.2 Decisiones que se cierran acá

1. **`/.obsidian` — sincronización selectiva.** Se sincroniza la configuración **portable** (`appearance.json`,
   `community-plugins.json`, `core-plugins.json`, `hotkeys.json`, `snippets/`, `themes/`, `plugins/*/manifest.json` y
   `plugins/*/data.json`); **no** se sincronizan los volátiles por dispositivo (`workspace*.json`, `.trash/`, cachés).
   Política inicial, ajustable con el uso real.
2. **Acceso de agentes (Fase 4).** Con E2E el servidor **no puede leer**: el agente es **un dispositivo más** —un
   cliente sin UI (CLI/servicio local) que tiene la passphrase y descifra **localmente**, con su propio par de claves
   registrado en el vault—. El servidor sigue sin ver texto plano.
3. **Coexistencia y migración.** LiveSync y `sync2` **no pueden correr sobre el mismo vault**. La migración es:
   exportar la historia de CouchDB (queda como respaldo, no se importa) → apagar LiveSync → resync inicial → el RAG y
   `obsn` apuntan al **dispositivo-agente** del punto 2.
4. **Backups: encajar, no duplicar.** El backup de Fase 2 se integra al flujo de exportación a S3 ya existente; no se
   inventa un segundo camino.

### 7.3 Riesgos que este plan asume explícitamente

- **CRDT vs CAS (la brecha funcional más grande).** El stack actual fusiona ediciones concurrentes **por
  construcción** (CRDT); este diseño es **CAS + oplog firmado** → un conflicto es **explícito**. La fusión es el motor
  de merge de **Fase 3**: hasta que exista, una edición concurrente sin conexión **se reporta**, no se resuelve sola.
- **Paridad de eventos del vault.** El estudio fue **estático**: cómo coalescen `vault.on('modify'/'rename'/'delete')`
  en uso real —y en móvil— **no está medido**. Es lo primero que mide el spike de Fase 3.
- **Superficie del cliente.** El core del cliente son ~200.000–400.000 líneas: cualquier expectativa de paridad de
  *features* está fuera de alcance; el objetivo es **sync confiable + motor propio**, no la paridad de la app.

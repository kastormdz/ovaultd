# ADR 0001 — Stack y alcance inicial

- **Fecha:** 2026-09-25
- **Estado:** aceptado

## Contexto

Se quiere paridad funcional con Obsidian Sync (servicio pago) con servidor propio. El riesgo real del
proyecto no es el server: es que el **cliente (plugin, JS) y el servidor (Rust) deriven las mismas claves**
y cifren de forma interoperable. Si eso falla, el proyecto no existe.

Además existe precedente de que **clonar el wire protocol propietario** termina mal:
`acheong08/obi-sync` (Go) fue roto a propósito por Obsidian en `>= 1.4.11` y archivado;
`CzBiX/ob-ovaultd-server` quedó atado a Obsidian 1.3.7.

## Decisiones

1. **Server en Rust** (axum + tokio + tokio-tungstenite + sqlx).
   - Un binario, sin GC, backpressure real en WebSocket, `object_store` para el CAS.
   - Coste: el plugin (TS) y el server (Rust) no comparten tipos → **la spec es el contrato**, y los
     vectores de prueba son la garantía (`docs/crypto.md`).
2. **Alcance inicial: fases 0→5 (MVP desktop).** Sync real + E2E + merge + history. Sin móvil, sin sharing.
3. **Cripto del plugin en JS puro** (`@noble/hashes` + `@noble/ciphers`), no WASM.
   - Corre igual en desktop y en móvil (Obsidian mobile no puede cargar addons nativos).
   - Verificado contra Rust en el spike.
4. **NO clonar el wire protocol del servicio oficial.** Se reimplementan las *decisiones de diseño*
   (revisiones monotónicas, hash-diff, tombstones, backoff), con protocolo propio.
   Los plugins de sync de terceros son legítimos (LiveSync, Sync Engine están en el registro oficial);
   lo que no es viable es hacerse pasar por el servicio oficial.

## Consecuencias

- Hay que mantener **vectores de prueba** de cripto en los dos lenguajes como test de regresión (ya existen).
- El plugin necesita su propio motor de merge (diff3) — no se puede reusar `diff-match-patch` del oficial tal cual.
- Migrar el MCP de Hermes (hoy sobre CouchDB) es una fase posterior (fase 9 del plan), no parte del MVP.

## Enmiendas

- **2026-09-25 - enmendado por ADR-0002:** la cripto del plugin deja de ser JS puro "porque no se puede usar WASM"
  y pasa a ser JS puro **por simplicidad y auditabilidad** (WASM no esta prohibido por el directorio de Obsidian).
  Los vectores `sync1` quedan como historicos: hay que re-ejecutar el spike con los parametros `sync2`.
- **2026-09-25 - enmendado por ADR-0002:** la consecuencia "el motor de merge es diff3 y no se puede reusar
  diff-match-patch" era incorrecta en ambos extremos: se usa **char-level diff-match-patch** con el algoritmo de
  solapamiento definido en `docs/protocol.md`, y **nunca** diff3.
- **2026-09-25:** `SQLite` (nivel A) es el default y lo unico a implementar; PostgreSQL (nivel B) y S3 (nivel C)
  son opt-in con interfaces ya definidas. Se elimina `region` del vault.

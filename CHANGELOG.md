# Changelog

Todos los cambios relevantes de este proyecto, por corte de release.
El formato sigue [Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/) y el versionado es
[semver](https://semver.org/lang/es/): server y plugin viajan en un **release train** (mismo
`MAJOR.MINOR` = pareja probada junta; ver ADR-0018 y la sección «Compatibilidad» del README).

## [0.2.11] — 2026-10-20

### Security

- **CORS por defecto SOLO loopback** (F-04 de la auditoría, cierre): sin
  `OVAULTD_ALLOWED_ORIGINS` el server ya no responde `Access-Control-Allow-Origin: *`;
  acepta únicamente orígenes `127.0.0.1` / `localhost` / `[::1]` con cualquier puerto
  (el runtime del CLI, cuyo puerto varía por perfil). El plugin usa `requestUrl` nativo y
  no depende de CORS; el panel es same-origin. Una página externa ya no puede leer
  respuestas autenticadas aunque tenga el token.
  `OVAULTD_ALLOWED_ORIGINS=*` queda como escape hatch explícito; un CSV sigue
  permitiendo exactamente esos orígenes. 5 tests en `tests/cors.rs`.

## [0.2.12] — 2026-10-20

### Changed

- **Panel: "Archivos", no "Notas".** El KPI y la columna cuentan todo lo que vive
  en el vault (markdown + adjuntos + config), no solo notas; el label ahora dice
  lo que el número es.

### Fixed

- **Barrido de uploads abandonados también al arrancar** (F-06 de la auditoría).
  El ticker solo corría cada hora, así que un `.part` huérfano de un crash
  esperaba hasta 60 minutos para limpiarse.

### Tests

- `la_ventana_vieja_se_descarta` estabilizado: espera por reloj hasta 5 s en vez
  de 1 s fijo, que bajo la carga de los tests de Argon2 se quedaba corto (flaky).

## [0.2.13] — 2026-10-22

### Fixed

- **CORS: el default conoce a los clientes reales.** El modo por defecto (sin
  `OVAULTD_ALLOWED_ORIGINS`) permitía solo loopback, y eso rompía el **plugin de
  Obsidian**: su core usa `fetch` desde `app://obsidian.md` (desktop) y
  `capacitor://localhost` (móvil), que no son loopback. El síntoma era
  `TypeError: Failed to fetch` en el plugin con el log del server vacío. Ahora el
  default incluye esos dos orígenes además de loopback; la web pública sigue
  cerrada. Test: `origenes_del_plugin_entran` en `tests/cors.rs`.

## [0.2.14] — 2026-10-24

### Security

- **`ctutils` 0.4.2 → 0.4.4** (RUSTSEC-2026-0385): la generación de máscaras y las
  comparaciones `u128` no eran constant-time, y `ctutils` es la base de
  `argon2`/`digest`, o sea el KDF del vault. **`tokio` 1.53.1 → 1.53.3**
  (RUSTSEC-2026-0386): `AsyncFd::new` no exigía ownership del fd. Las dos con fix
  de patch; `osv-scanner` vuelve a 0 issues (el lock del spike incluido).

### Fixed

- **Panel: "última actividad" refleja el sync real, no el último login.** `last_seen`
  solo se actualizaba en `autenticar_token` (requests HTTP) con un throttle de una
  hora, y el sync de verdad va por WebSocket con firma Ed25519, que no pasa por ahí:
  un device sincronizando cada 5 s aparecía con la fecha del último login ("hace 2
  días"). Ahora el commit del batch también late (throttle 60 s), el HTTP late cada
  minuto, y el panel toma el **más reciente** entre `last_seen` y el último batch
  (`max(...)` en vez de `COALESCE`, que priorizaba un `last_seen` viejo). Test:
  `batch_valido_da_batch_ack_con_la_rev_y_una_sola_revision` verifica el latido.

## [0.2.15] — 2026-10-24

### Fixed (pérdida de datos)

- **Un batch con dos operaciones del mismo path ya no deja el device sin poder
  pushear.** `revisions` tiene PK `(vault_id, rev, path_id)` y el commit insertaba una
  fila por operación: dos ops del mismo path (un `pending` re-editado, `del`+`add` del
  mismo nombre, o `move`+`mod` del protocolo) violaban la PK, el commit devolvía 500 y
  el reintento idéntico volvía a fallar. Ahora se aplica la última op de cada path y un
  `move`+`mod` colapsa en una sola fila conservando el `prev_path_id` (y tombstoneando
  el origen). Tests: `dos_ops_del_mismo_path_colapsan_en_una_revision` y
  `move_mas_edit_en_una_revision_conserva_el_origen_borrado`.

- **Un archivo que no se puede leer ya no se propaga como borrado.** `stat` devolvía
  `null` ante cualquier error (EACCES, EIO, ELOOP) y el scan lo leía como "no existe":
  el push publicaba `del` y todos los devices borraban esas notas. Ahora esos paths se
  marcan `ilegibles` y el diff los excluye (ni suben ni se borran este ciclo). Test:
  un `readMany` que falla en 1 de 3 ya no produce ningún `del`.

- **El cursor del pull avanza sólo con la revisión completa.** `completeApply` escribía
  `since = rev` por cada registro, pero una revisión del server trae N operaciones y el
  delta pagina `rev > since`: si la op 2 fallaba, las demás no se entregaban nunca (y
  `prune-remote` después las borraba del server). Ahora `pullOnce` cierra el cursor al
  terminar todas las ops de la revisión, `writeCheckpoint` nunca retrocede y conserva
  `minRev`. Tests: revisión de 3 ops (avanza a la 3.ª) y bloqueo en la 2.ª (no avanza).

- **El import del plugin escribe los bytes exactos.** `Buffer#slice()` no copia: su
  `.buffer` es el pool de 8 KB de Node, así que todo archivo menor a 4 KB se escribía
  con 8192 bytes (con memoria ajena) y se publicaba así. `copiaExacta()` copia en un
  `Uint8Array` del tamaño justo; lo usan el import y el adapter. Test con un Buffer del
  pool (100 B reales → 100 B, no 8192).

### Fixed

- **Panel: última actividad con el más reciente real.** `max(a, subselect)` de dos
  argumentos devuelve NULL si falta uno (un device read-only mostraba "sin actividad");
  ahora es `NULLIF(max(COALESCE(batch), COALESCE(last_seen)), '')`.

## [0.2.16] — 2026-10-24

### Fixed

- **El panel mostraba las fechas en UTC como si fueran locales.** `datetime('now')` es
  siempre UTC y las tablas imprimían el string crudo: en Argentina la "última actividad"
  aparecía tres horas adelantado. Ahora todas las fechas del panel (devices, vaults,
  cuentas, auditoría y la actividad del dashboard) se convierten al huso configurado con
  `OVAULTD_TZ_OFFSET_MINUTES` (minutos respecto de UTC, `-180` = UTC-03:00), y cada
  columna rotula la zona. Sin la variable se muestra UTC, explícito. La base sigue en UTC.
  La conversión es aritmética civil sin dependencias (la imagen no garantiza `tzdata`).

## [0.2.17] — 2026-10-24

### Fixed

- **`OVAULTD_TZ_OFFSET_MINUTES=-180` tumbaba el arranque.** El lector de enteros de la
  configuración exige `> 0`, y el huso es negativo a propósito: el server entraba en
  restart loop con el valor correcto para Argentina (UTC-03:00). Ahora el huso usa un
  lector con rango explícito (`-1440..1440`) y hay tests de `Config::construir` con
  `-180`, `0`, `330`, `-1440` y los fuera de rango. Los literales de `Config` de los
  tests no probaban el parseo: por eso el bug pasó los gates.

## [0.2.18] — 2026-10-24

### Fixed

- **El CAS perdido llega como `conflict`, no como 500.** El server nunca emitía
  `{"t":"conflict"}`: un `expected_rev` viejo salía como `error/internal` y **cerraba el
  socket**, así que el cliente abortaba el pull→push entero y una escritura concurrente de
  otro device congelaba la sincronización. Ahora responde `conflict` con
  `path_id`/`base_rev`/`server_rev`/`client_rev` (la spec) y la conexión sigue viva.
- **`expected_rev` se valida dentro de la transacción.** La transacción toma el write lock
  al abrir (un `UPDATE` no-op) y relee el head adentro: dos commits concurrentes ya no
  leen la misma foto, y el perdedor recibe `conflict` en vez de chocar con la PK de
  `batches` o con `BUSY_SNAPSHOT`. Test con `tokio::join!`: uno gana, el otro es
  `Conflict`, y no quedan filas a medias.
- **Errores no fatales no cortan el socket.** `bad_request`, `not_found` y cuota se
  responden como `error` y la sesión sigue; sólo `unauthorized` (credenciales) e
  `internal` (estado en duda) cierran. El `hello` sí cierra en cualquier error.

## [0.2.21] — 2026-10-24

### Fixed

- **Un conflicto estructural ya no congela el dispositivo.** `pullOnce` cortaba el ciclo en
  la primera revisión `blocked` y el checkpoint no avanzaba: una sola carpeta en conflicto
  dejaba al device sin bajar **nada más**, para siempre. Ahora el intent queda `blocked`
  (visible y resoluble desde el Resolver) y el resto del vault sigue bajando. El report
  expone `bloqueados[]`.
- **El conflicto carpeta ↔ archivo se conserva en vez de bloquear.** Si el remoto trae una
  carpeta donde hay un archivo (o al revés), el local se preserva: el archivo se copia a
  `… (conflicto <device> <fecha>)` y el **directorio se renombra entero** (atómico, con sus
  hijos). Antes quedaba bloqueado esperando que un humano lo tocara.
- **Aplicar una revisión `folder` nueva explotaba** con "blobId es obligatorio salvo en un
  borrado": una carpeta no tiene blob y el intent la rechazaba antes de crear nada.

## [0.2.20] — 2026-10-24

### Added

- **La versión del cliente en el panel.** El `hello` acepta `app_version` (opcional) y el
  server la guarda por device: la tabla de dispositivos muestra qué plugin/CLI corre cada
  uno. Un cliente viejo no la manda y la columna queda en "—".
- **`configDir` real en el escaneo.** Con un config dir custom (`.obsidian-work`) se
  excluye ESE y no un `.obsidian` fijo: antes se subía la config entera del vault, tokens
  de plugins incluidos.

### Fixed

- **`appearance.json` ya no se aplica tal cual.** El pull mezcla por campo: los portables
  (tema CSS, snippets, tamaño de fuente) vienen del remoto y el modo claro/oscuro, las
  fuentes y las claves desconocidas quedan del dispositivo. Antes un pull te cambiaba el
  tema del día. El índice y la base guardan el hash de lo publicado, así que el tema local
  no vuelve a subir en cada ciclo.
- **La outbox tiene cuarentena** (ver plugin 0.2.34): una operación impublicable no congela
  la cola.

## [0.2.19] — 2026-10-24

### Fixed

- **Los snapshots con `rev` explícito ahora son el estado real de esa revisión.** El
  manifest se armaba desde `files` (el estado ACTUAL) filtrando `rev <= R`, así que
  omitía todo path modificado o borrado después de R: un snapshot de la rev 10 se
  listaba como punto de restauración y no contenía la mitad de las notas. Ahora sale del
  **oplog** (la última operación de cada path con `rev <= R`, sin los `del`), que es lo
  que el struct prometía desde el principio. Test: A (rev 1) borrada en la 4 y B (rev 2)
  editada en la 3, snapshot en la 2 → incluye A y la versión de B de la rev 2, y no el
  blob de la 3.
- **Un snapshot que referencia un blob ya recolectado falla en vez de mentir.** El
  `UPDATE blobs SET refs = refs + 1` no miraba `rows_affected`: si el GC se había
  llevado el blob, el snapshot se servía como restaurable. Ahora devuelve error.

## [Unreleased]

### Security

- **Bridge del CLI: `proxyFetch` con allowlist** (F-01 de la auditoría 2026-10-20).
  El puente reenviaba a cualquier URL tomada de un header; ahora solo al **mismo
  origen que el server de la sesión** (`serverUrl`), con esquema `http`/`https`, y
  **fail closed** si no hay server configurado. No reenvía `x-ovaultd-*`, `cookie`,
  `host`, `connection`, `transfer-encoding` ni `content-length`. Tests:
  `proxy-allowlist.test.ts`.
- **Bridge del CLI: token por sesión en `/fs/*`** (F-02). El puente aceptaba
  operaciones de cualquier proceso local (leer y borrar el vault sin credenciales).
  Ahora exige un token de 32 bytes generado por sesión, comparado en tiempo
  constante, que SOLO recibe el Chromium del runtime — inyectado con `route` sobre
  `/fs/**` y no con `setExtraHTTPHeaders` (un header global viaja también al server,
  dispara un preflight CORS con un header no permitido y rompe el sync con
  `Failed to fetch`). Sin token: **401**. Tests: `bridge-token.test.ts`.
- **`prune-remote`: guarda de "disco vacío"** (F-03). Si el disco local tiene 0
  archivos y el server tiene paths, el plan borraría el vault entero: la comanda
  aborta antes de imprimirlo. Escape hatch explícito: `--permitir-vacio`.
  Tests: `prune-guard.test.ts`.
- **SCA: la excepción de `osv-scanner.toml` vence** (F-05) con `ignoreUntil =
  2027-01-31`: si `basic-toml`/`askama` cambian, el gate vuelve a romper solo.

### Added

- **CLI: `prune-remote`** — reconcilia los paths vivos del server contra el disco local.
  El server es ciego (E2EE): un borrado local sin sync deja el path vivo en `files` y el
  panel lo cuenta de más. Baja el oplog completo, lo colapsa por `path_id`, descifra cada
  ruta y la compara contra el filesystem. Dry-run por defecto; `--yes` encola un `del` por
  huérfano. `--json` para scripts.
- **CLI: `sync --rescan`** — ignora el cache del escaneo y relee/hashea todo el vault.
  `--rescan-every <n>` lo hace periódico en `--continuous` (el 1er ciclo siempre rescan):
  el servicio usa 60 (~5 min) para corregir una desalineación entre el índice y el disco
  sin pagar O(vault) en cada pasada.
- **CLI: `cat`, `write`, `ls` y `rm`** sobre el vault local (`packages/cli/src/vault-fs.ts`).
  Es la base del dispositivo-agente de la fase 4: el vault sincronizado se edita como
  archivos y el `sync` propaga; las rutas se validan con `validarRutaDeVault` y no pueden
  escapar del vault (travesía, absolutas ni symlinks). `write` toma el contenido por
  `--stdin` (markdown multilínea) o `--file`, y publica con rename atómico.
- **El CLI pide la password del vault UNA sola vez**: las subclaves derivadas se cachean
  en el estado local (0600, invalidado por epoch) y `ovaultd olvidar-clave` las borra.
  Es lo que permite `sync` desde un timer sin TTY.
- **Comando `ovaultd` instalable**: `scripts/instalar-cli.sh` crea el shim en
  `~/.local/bin` (el paquete es privado y no va al registry) y `docs/cli.md` documenta
  instalación, vínculo, sync, comandos de filesystem e historia/restore.

## [0.2.10] — 2026-10-19

Remediación de la segunda auditoría (F-01…F-11). Sin cambios de contrato: wire `v2`, API
`v1` y el comportamiento del cliente legítimo quedan igual.

### Fixed

- **F-01 (High, pérdida de datos) — el GC borraba del CAS un blob vivo de otro vault.**
  El CAS es global por hash; `gc::planificar` ahora excluye los hashes con más de una fila
  `blobs` y `gc::borrar` re-verifica `COUNT(*) WHERE hash = ?` dentro de su transacción.
  Antes, dos vaults con el mismo contenido + `gc --delete` dejaban al vault sano en 404 con
  su índice mintiendo.
- **F-02 — `changes` traía el oplog entero a RAM.** Corte por revisión: se miran los
  tamaños (una fila por revisión) y se traen solo las filas que entran; `limit = 0` se
  clampea a 1 (antes significaba «traeme todo»). La primera revisión entra siempre, aunque
  sola supere el limit.
- **F-03 — snapshots sin tope.** 100 snapshots y 64 MB de manifests por vault (reserva
  atómica en el INSERT) + etiqueta ≤ 128 caracteres.
- **F-06 — Argon2id con parámetros explícitos** (m=64 MiB, t=3, p=4; antes `default()`,
  m=19 MiB t=2) y política de password de **12** caracteres + lista local de comunes. Los
  hashes existentes siguen verificando (el verify lee los parámetros del PHC).
- **F-08 — topes de nombre** (device/platform 128, vault 256, sin saltos de línea).

### Added

- **`OVAULTD_ALLOWED_ORIGINS`** (CORS): CSV de orígenes; vacío = `*` (desarrollo).
- **`OVAULTD_PUBLIC_HOST`**: host que devuelve `/access` (detrás de un borde el `Host` del
  request es spoofeable y el cliente persiste lo que aprende).
- **F-10**: `GET /vaults` devuelve el rol real de `vault_members` (fallback `owner`).
- **F-11**: barrido de subidas abandonadas cada hora (antes solo al crear una subida).

### Changed

- **F-09**: WebSocket con timeout de inactividad de 5 minutos y máximo 4 transferencias
  simultáneas por conexión.
- **F-07**: `backup.sh` usa `bash -c --` en vez de `eval` (elimina la doble expansión).

### Tests

- `tests/auditoria_2026_10_19.rs`: regresión de F-01, F-02, F-03, F-06, F-08 y F-10.
- Server **197/197**, workspace **227/227**, `xtask protocol-check` verde.

## [0.2.9] — 2026-10-19

### Changed

- **Durabilidad del CAS por lote**: `publish` ya no hace fsync por blob (archivo +
  directorio); el commit del lote sincroniza el CAS entero con un `syncfs` ANTES del ack.
  Un blob confirmado sigue sobreviviendo al crash; uno sin commit se pierde sin
  consecuencias (el cliente lo re-sube: los blobs son inmutables y el PUT idempotente).
- **`TCP_NODELAY`** en el socket de escucha (los aceptados heredan el flag) y **`max_age`**
  en el CORS: menos espera por delayed ACK y el preflight se cachea una hora.

## [0.2.8] — 2026-10-19

Higiene de dispositivos en el panel: la lista muestra solo los activos y el dispositivo se
presenta con el nombre real de cada máquina en vez del literal «obsidian».

### Added

- **Latido de dispositivos** (`routes.rs::autenticar_token`): cada request autenticado por
  token actualiza `devices.last_seen` (throttled a una hora por dispositivo, best-effort:
  un fallo del UPDATE no frena el request). Es el dato que distingue un device en uso de un
  vínculo abandonado.
- **`/admin/devices` filtra por activos**: por defecto solo lista los devices no revocados
  que dieron señales de vida (`last_seen` o al menos un lote commiteado). Los inactivos se
  informan con su conteo y quedan a un click (`?todos=1`) para auditarlos o revocarlos.
- **KPI `dispositivos` del dashboard**: mismo criterio que la lista (no cuenta vínculos
  abandonados).

### Fixed

- **Lista de dispositivos vacía con inactivos ocultos**: el conteo y el enlace de
  «Ver también inactivos» ahora se muestran también cuando no hay activos (el estado vacío
  ya no se los come).

### Tests

- `devices_oculta_los_inactivos_y_los_muestra_con_todos`: cubre el filtro, el conteo y el
  enlace de inactivos.

## [0.2.7] — 2026-10-18

Fase 5, corte 5e: GC de blobs huérfanos, con auditoría del invariante de referencias.

### Added

- **`ovaultd-admin gc`** (`crates/ovaultd-server/src/maintenance/gc.rs`): limpia los
  blobs que nadie referencia — publicados sin batch que los commitee, o cuyo único
  dueño era un snapshot ya borrado. Tres modos:
  - `gc` (dry-run, el default): lista candidatos con tamaño y fecha; no escribe nada.
    La gracia (24 h, `--gracia-minutos`) protege a los blobs recién publicados, cuyo
    `refs` todavía es 0 hasta que aterrice su commit.
  - `gc --verify`: audita el invariante `refs = revisiones con blob + snapshots`
    recontando las fuentes y comparando en ambos sentidos (detecta drift del contador
    y referencias colgadas). Solo lectura.
  - `gc --delete`: borra filas + archivos del CAS, en una transacción, excluyendo los
    uploads en curso (`offset < total`). Con el server parado.
- `--vault N` limita cualquier modo a un vault.

### Tests

- `tests/history_gc.rs` (6 casos): huérfano viejo → borrado real de fila y CAS
  (idempotente); gracia protege al recién publicado; una revisión retiene al blob para
  siempre; `snapshot_only_reference_survives_gc` (y borrar el snapshot devuelve la
  referencia sin romper el invariante); el borrado saltea uploads en curso; `--verify`
  detecta drift y referencias colgadas.

## [0.2.6] — 2026-10-18

Fix de una carrera del handshake: dos conexiones paralelas del mismo dispositivo ya no se pisan.

### Fixed

- **El `hello` del WS emite SIEMPRE un challenge nuevo.** Reusaba el challenge pendiente del
  dispositivo, así que dos conexiones simultáneas del mismo device (el motor del plugin y una
  lectura al vuelo — historia, resolver, export) firmaban el MISMO challenge: la primera lo
  consumía al autenticar y la segunda quedaba rechazada con `unauthorized`. El motor del plugin
  moría al recargar con una lectura en paralelo (carrera reproducida en producción el
  2026-10-18); ahora cada conexión tiene su challenge y el retry re-firma el que acaba de
  recibir. Covered-by: `ws::tests::dos_conexiones_paralelas_del_mismo_device_autentican_las_dos`.
- El endpoint HTTP `/challenge` + `/access` conserva el reuso a propósito: el proof lleva el
  challenge dentro de la firma, así que el server DEBE buscar el pendiente no consumido para
  verificar (flujo secuencial del CLI).

## [0.2.5] — 2026-10-18

Fase 5, cortes 5a+5b: historia por archivo y snapshots (server) + el cliente de historia
(núcleo y CLI).

### Added

- **API de historia** (`GET /vaults/{id}/history`): el oplog por `path_id` (hex), más nueva
  primero, paginado (`before`), con la cadena de renames (`prev_path_id`) resuelta por el
  server (CTE recursivo acotado). El server sigue ciego: `path_id`/`blob_hash` viajan en hex,
  `path_enc`/`sig`/`t_mac` en base64.
- **Snapshots etiquetados** (`GET/POST /vaults/{id}/snapshots`, `GET/DELETE .../{snap}`):
  corte coherente en `rev` (clampeado al head), manifest `{path_id: blob_id}` armado desde
  `files` vivos, y contabilidad de `blobs.refs` en la MISMA transacción (el invariante que el
  GC de la fase 5 va a respetar; borrar un snapshot devuelve sus referencias).
- **`ovaultd history <path>`**: cada revisión (op, fecha, dispositivo, tamaño), con
  paginación (`--before`/`--limit`); funciona para archivos borrados y nombres anteriores a
  un rename.
- **`ovaultd preview <path> --rev N [--diff]`**: descarga, VERIFICA (sha256 del blob contra
  su `blob_id` antes de descifrar) y muestra una revisión sin escribir en el vault; `--diff`
  la compara contra el archivo local.

### Changed

- El runtime del núcleo pasa a `0.6.0` (métodos `history.list`, `history.preview`,
  `history.diff`).

## [0.2.4] — 2026-10-18

Segundo bloque del plan de remediación (P1 de la auditoría del 2026-10-18): higiene de la
superficie de operación y de la cadena de suministro del CI.

### Changed

- **`/metrics` sale de `OVAULTD_METRICS` y queda apagado por defecto** (`off`): la ruta no
  existe (404) y el endpoint deja de ser público sin pedirlo. `localhost` responde solo al
  peer TCP de loopback (el acceso directo al puerto: un request que llegó por el borde no lo
  es, aunque su `X-Forwarded-For` diga `127.0.0.1`); `on` la expone como el resto del API y
  conviene cortarla en el borde (`docs/deploy.md` trae el `location /metrics { deny all; }`).

### Fixed

- **`auth_challenges` se barre al emitir**: los vencidos se limpian en cada emisión (perezoso,
  el patrón de `uploads::barrer`, sobre el índice `idx_challenges_expira` que ya existía).
  Sin el barrido, un cliente sin credenciales podía hacer crecer la tabla con `hello`s que
  nunca consume.
- **Las Actions del CI van pinneadas por SHA** (commit completo + comentario de la versión) y
  el gate corre **`osv-scanner`** (versión fijada; el binario se verifica contra su
  `SHA256SUMS` oficial) sobre `Cargo.lock` y los `package-lock.json`: con hallazgos, el gate
  cae y la release no se publica. Dependabot mantiene los pins actualizados.

## [0.2.3] — 2026-10-18

Corrección del P0 sobre la 0.2.2, cazada por la verificación del deploy en producción.

### Fixed

- **La IP del cliente con `TRUST_PROXY=1` tomaba el PRIMER valor del `X-Forwarded-For`**: los
  bordes APPENDEAN (nginx y NPM Plus: `$proxy_add_x_forwarded_for`), así que el primer valor lo
  controla el cliente y rotarlo seguía esquivando el freno de login — el mismo vector que la
  0.2.2 quiso cerrar. Ahora vale el ÚLTIMO valor del XFF (el que agregó el borde) y `X-Real-IP`
  gana si está (los bordes que lo setean lo pisan). Cazado EN VIVO contra producción: con el
  spoof en el primer valor, la auditoría guardaba la IP inventada en lugar de la real.

## [0.2.2] — 2026-10-18

Parche de seguridad sobre la 0.2.1 (primer bloque del plan de remediación de la auditoría del
2026-10-18).

### Fixed

- **Freno de fuerza bruta en el API de login**: `POST /api/v1/auth/signin` no tenía límite (solo
  el panel web lo tenía). Ahora usa el mismo freno (5 intentos por minuto y por `ip|email`, `429`
  con `Retry-After`, auditado), y los intentos contra cuentas inexistentes también cuentan: el
  password spraying y la quema de CPU por Argon2 quedan cortados sin afectar los usos legítimos
  (el plugin y el CLI hacen signin de baja frecuencia).
- **Los headers de un proxy ya no son de cualquiera**: `X-Real-IP` / `X-Forwarded-For` /
  `X-Forwarded-Proto` se obedecen SOLO con el nuevo `OVAULTD_TRUST_PROXY=1` (default `0`); sin el
  flag, la IP del cliente sale del peer TCP real y el `Secure` de la cookie no depende de un
  header. Antes, rotar `X-Forwarded-For` esquivaba el freno de login y un
  `X-Forwarded-Proto: http` abría la cookie del panel.

  **Si el server está detrás de un borde (NPM/nginx/Caddy), activá `OVAULTD_TRUST_PROXY=1`**
  (ver `docs/deploy.md`): sin el flag el freno sigue funcionando por cuenta, pero pierde la IP
  real del cliente y la cookie queda conservadora (sin `Secure`).

## [0.2.1] — 2026-10-18

Parche de corrección sobre la 0.2.0, más la primera pieza del sync en vivo: el server avisa a los
demás dispositivos del vault apenas alguien commitea.

### Added

- **Aviso entre dispositivos (`notify` por WebSocket)**: al commitear un lote, el server reenvía el
  registro a las demás conexiones del mismo vault. El aviso sale del oplog recién commiteado, así
  que no puede mentir sobre lo que hay, y es **sólo una señal**: quien lo recibe hace un pull con su
  `since`, de modo que un aviso perdido no pierde ningún dato. Cubierto por un test con dos
  dispositivos reales del mismo vault.

### Fixed

- **`/setup` sin estilos**: la pantalla de la primera cuenta se armaba como HTML a mano, con estilos
  inline, y salía pelada (ni siquiera pedía el CSS del panel). Ahora usa la plantilla
  `base_centrado.html`, la misma de login y registro, y sus textos viven en `admin/textos.rs`, como
  el resto de la interfaz.
- **Los botones del panel tenían el texto pegado a la izquierda**: `.boton` es `inline-flex` y no
  declaraba `justify-content`, así que el `text-align` del navegador no aplicaba.
- **Verificación de leaks**: la etiqueta de interfaz «Password de la cuenta» se veta por nombre,
  como el resto del glosario de textos de UI.

## [0.2.0] — 2026-10-17

Primera release publicada: el server sale del «funciona en mi lab» y pasa a tener panel de
administración, contrato de compatibilidad y distribución por releases.

### Added

- **Panel web de administración** (server-rendered, sin CDNs ni Node; funciona sin JavaScript):
  - Cuentas con roles: crear, deshabilitar/habilitar, promover/degradar, links de
    restablecimiento de un solo uso (24 h) y borrado con purga de sus vaults.
  - Vaults: listado con contadores por vault, detalle (dueño, `enc_version`, generación de
    llave, cuota, notas, revisiones, lotes, fragmentos, bytes, miembros, últimos lotes) y
    borrado con confirmación por **nombre exacto**.
  - Dispositivos: listado global con tokens vivos y última actividad, y revocación instantánea
    (mata el dispositivo y todos sus tokens).
  - Auditoría paginada con filtro por acción o detalle.
  - Ajustes: abrir/cerrar el registro público. Cambio de password propio.
- **`GET /api/v1/meta`** (público): contrato de compatibilidad con `server_version`,
  `api`, `wire_protocol`, `min_plugin_version` y `features`.
- **Borrado de vault por API** (`DELETE /api/v1/vaults/{id}?confirm=<nombre>`, con cascada y
  barrido del CAS) y **re-sellado de material** (`PUT /api/v1/vaults/{id}/material`).
- Cabeceras de seguridad en todo el panel: CSP estricta (`default-src 'none'`), `nosniff`,
  `referrer-policy`, `permissions-policy` y `frame-ancestors 'none'`.
- CORS: se permite el header `x-ovaultd-client` con el que los clientes se identifican.

### Changed

- **Diseño del panel**: «command center» con rail lateral, veredicto de salud del servidor
  (uptime, tamaño de la base, almacén cifrado, última sincronización), contadores, últimas
  acciones y estados vacíos. Todo el texto visible vive en un módulo de textos (`textos.rs`),
  listo para un segundo idioma sin tocar plantillas.
- Los assets del panel se sirven con `ETag` + `no-cache` (y URL versionada al cambiar el
  diseño): un HTML nuevo contra una hoja cacheada dejaba el panel sin estilo.
- Responsive real: el rail envuelve en pantallas chicas y las tablas anchas se deslizan dentro
  de su tarjeta.
- El panel muestra solo lo accionable: los parámetros de arranque (que se configuran por
  entorno) salieron de «Ajustes».

### Fixed

- Mensajes del panel con acentos: iban pre-codificados a mano y se re-codificaban al doble
  (`+` literal en pantalla).
- La guarda del `apply` compara contra el hash del contenido descifrado: un bloqueo en falso
  de un local que ya coincidía con el remoto ahora converge.

### Security

- El server sigue siendo **ciego**: nunca recibe `vault_key`, KEK ni recovery, y no descifra
  contenido. El panel administra cuentas y metadatos, no llaves del vault.

[0.2.0]: https://github.com/kastormdz/ovaultd/releases/tag/server-v0.2.0

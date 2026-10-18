# Contrato de interfaces de `ovaultd-server` (lo que el servidor expone y lo que NO ve)

Este archivo es el **contrato**: los nombres y las firmas están congelados para que cada archivo se pueda escribir
por separado y compile contra los demás. Lo que está acá **no se negocia**; los cuerpos se implementan.

## Regla de oro del servidor

El servidor **jamás** recibe `vault_key`, la KEK ni el secreto de recovery, y **nunca** descifra un `path_enc` ni un
`blob`. Lo único que hace con cripto es **verificar**: `ovaultd_crypto::verify_record(...)` con la `vault_pubkey` del
vault. Si un cuerpo de código necesita `k_*` para hacer su trabajo, el diseño está mal, no el contrato.

## Módulos

```
crates/ovaultd-server/src/
  main.rs            CLI (clap) + armado de dependencias + arranque de axum
  lib.rs             declaración de módulos, AppState y Config            <- YA ESCRITO (interfaz)
  config.rs          Config::from_env()                                    <- Luna
  error.rs           ServerError, Result, mapeo a las 7 ErrorCode          <- Luna
  db.rs              pool + PRAGMAs + migrate()                            <- Luna
  storage.rs         CasStore: publicar/leer blobs                         <- Luna
  auth.rs            cuentas, devices, challenge, verificación de firma    <- Luna
  routes.rs          router axum: /healthz, /metrics, /setup, vaults, push, pull   <- Luna
  admin/             panel web de administración (ADR-0017): sesiones, login, páginas
  sync/mod.rs        pub mod commit;                                      <- YA ESCRITO
  sync/commit.rs     la ÚNICA ruta de escritura (transacción atómica)      <- Luna
  delta.rs           paginado por revisiones COMPLETAS                     <- Luna
```

## Firmas congeladas

```rust
// config.rs
#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: String,          // OVAULTD_BIND,    default "127.0.0.1:8080"
    pub database_path: String,      // OVAULTD_DB,      default "./.tmp/server/metadata.db"
    pub cas_root: PathBuf,          // OVAULTD_CAS,     default "./.tmp/server/cas"
    pub token_ttl_hours: i64,       // OVAULTD_TOKEN_TTL_HOURS, default 720
    pub challenge_ttl_seconds: i64, // OVAULTD_CHALLENGE_TTL,   default 300
}
impl Config { pub fn from_env() -> Result<Self>; }

// error.rs
pub type Result<T> = std::result::Result<T, ServerError>;
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("no autorizado")]            Unauthorized,
    #[error("prohibido")]                Forbidden,
    #[error("no encontrado")]            NotFound,
    #[error("conflicto en rev {expected}, head {head}")] Conflict { path_id: PathId, expected: u64, head: u64 },
    #[error("código de protocolo: {0}")] Protocol(ErrorCode),   // las 7 de ovaultd-protocol
    #[error("entrada inválida: {0}")]    Invalid(String),
    #[error(transparent)]                Db(#[from] sqlx::Error),
    #[error(transparent)]                Io(#[from] std::io::Error),
}
impl ServerError { pub fn error_code(&self) -> Option<ErrorCode>; }  // para el JSON {"t":"error","code":...}
// MAPEO (estas dos reglas NO se negocian; el doc manda):
//   Forbidden -> Some(Unauthorized)   // protocol.md: `unauthorized` cubre "dispositivo revocado, firma de auth
//                                     // invalida, ACL ajena". No existe codigo de permiso separado.
//   Conflict  -> None                 // y el MENSAJE lleva el `path_id` REAL del push: un campo de 32 bytes en
//                                     // cero es una mentira que PARECE valida (el cliente no sabria que archivo
//                                     // perdio la carrera). un conflicto de revision es un MENSAJE ({"t":"conflict",...}) con HTTP 409,
//                                     // NO un codigo de error: mandar `internal` convierte un resultado normal
//                                     // del compare-and-swap en "bug del servidor". Campos: ADR-0008 (G-11).
impl axum::response::IntoResponse for ServerError { .. }             // status + cuerpo del protocolo

// db.rs
pub async fn connect(path: &str) -> Result<SqlitePool>;   // WAL + foreign_keys + busy_timeout=5000, max 8 conns
pub async fn migrate(pool: &SqlitePool) -> Result<()>;    // sqlx::migrate!("./migrations")

// storage.rs
pub struct CasStore { /* ... */ }
impl CasStore {
    pub fn new(root: PathBuf) -> Result<Self>;
    /// Escribe el blob de forma atómica y DURABLE: temp en el mismo FS -> valida longitud y hash -> fsync del
    /// archivo -> rename atómico -> fsync del DIRECTORIO. Devuelve el hash verificado.
    pub async fn publish(&self, hash: &BlobId, bytes: &[u8]) -> Result<()>;
    pub async fn read(&self, hash: &BlobId) -> Result<Vec<u8>>;
    pub fn exists(&self, hash: &BlobId) -> bool;
}

// auth.rs
pub struct Cuenta { pub id: i64, pub email: String }
pub async fn crear_cuenta(pool: &SqlitePool, email: &str) -> Result<Cuenta>;
pub async fn emitir_device(pool: &SqlitePool, account_id: i64, name: &str, platform: Option<&str>) -> Result<String>;
/// Devuelve el device_id si el token es válido, no venció y no fue revocado.
pub async fn verificar_token(pool: &SqlitePool, token: &str) -> Result<i64>;
pub async fn revocar_device(pool: &SqlitePool, device_id: i64) -> Result<()>;
/// Challenge aleatorio de un solo uso, atado a (device, vault) y con vencimiento.
pub async fn emitir_challenge(pool: &SqlitePool, device_id: i64, vault_id: i64) -> Result<Vec<u8>>;
pub async fn consumir_challenge(pool: &SqlitePool, device_id: i64, vault_id: i64, challenge: &[u8]) -> Result<()>;

// sync/commit.rs — LA ruta de escritura, una sola transacción
pub struct CommitRequest<'a> { pub vault_id: i64, pub device_id: i64, pub push: &'a Push, pub blob: Option<&'a [u8]> }
pub struct CommitResult { pub rev: u64, pub ack: PushAck, pub repetido: bool }
pub async fn commit(state: &AppState, req: CommitRequest<'_>) -> Result<CommitResult>;

// delta.rs
#[derive(Debug, serde::Serialize)]
pub struct Delta { pub changes: Vec<Changes>, pub next_since: u64, pub min_rev: u64 }
/// Pagina por REVISIONES COMPLETAS: si `limit` corta una revisión de varias filas, se devuelve completa igual.
pub async fn delta(pool: &SqlitePool, vault_id: i64, since: u64, limit: u32) -> Result<Delta>;

// routes.rs
pub fn router(state: AppState) -> axum::Router;
```

## Regla de SQL (ADR-0010)

Se usa la **API de ejecución** de SQLx con el tipado en `#[derive(sqlx::FromRow)]`:

```rust
#[derive(sqlx::FromRow)]
struct FilaCuenta { id: i64, email: String }
let fila = sqlx::query_as::<_, FilaCuenta>("SELECT id, email FROM accounts WHERE id = ?")
    .bind(id).fetch_optional(pool).await?;
```

**PROHIBIDO `sqlx::query!` / `query_as!`**: verifican el SQL contra una base viva en tiempo de compilación, y con eso
`cargo check` (y el CI, y el que llega nuevo) necesita un servicio levantado para compilar. El tipado no se pierde:
viene de `FromRow`. Todo por `.bind(...)`, nunca SQL armado con `format!`. Ver `docs/decisions/0010`.

### Ids generados: `RETURNING id`, nunca un `last_insert_rowid()` aparte

```rust
let id = sqlx::query_scalar::<_, i64>("INSERT INTO accounts (email) VALUES (?) RETURNING id")
    .bind(email).fetch_one(pool).await?;
```

`last_insert_rowid()` es **por conexión** y SQLx toma cualquier conexión del pool para cada sentencia: el `SELECT`
aparte puede caer en otra conexión y devolver **0**. El bug es invisible hasta que una FK lo delata
(`FOREIGN KEY constraint failed` con un `account_id = 0` fantasma). Una sentencia, una conexión, un id.

## Regla de los tests (no reimplementar la produccion)

Los tests abren la base con `crate::db::connect(...)`. **Prohibido** armar la conexion a mano dentro de un test
(`SqlitePoolOptions::new().connect("sqlite://...")`): eso pierde `create_if_missing`, WAL, `foreign_keys` y
`busy_timeout`, y el test termina fallando por un motivo ajeno al codigo que dice probar (real: `code 14 unable to
open database file`). La regla general: **un test usa el mismo camino que produccion, o no prueba produccion.**

## Commit del batch — `sync/commit.rs` (ADR-0008, opcion A)

La **unica** ruta de escritura del servidor. Un mensaje `batch` = una transaccion = **una** revision.

### Firma congelada

```rust
#[derive(Debug, Clone, PartialEq)]   // los tests lo comparan con assert_eq!/unwrap: sin derive no compilan
pub struct CommitOutcome {
    pub rev: i64,
    pub idempotente: bool,   // true: el operation_id ya estaba, no se escribio nada
}

/// Aplica el batch completo. O entra entero, o no entra nada.
pub async fn commit_batch(
    pool: &SqlitePool,
    vault_id: i64,
    device_id: i64,
    batch: &Batch,
) -> Result<CommitOutcome>;
```

`Batch`, `BatchOp` y `Op` los define `ovaultd-protocol`; **no se redefinen aca**. `batch.operation_id` se guarda como el
**string tal cual llega** (TEXT) para que la busqueda de idempotencia sea una igualdad exacta.

### Orden de validacion: el primer fallo gana y NO se escribe nada

| # | Chequeo | Error |
|---|---|---|
| 1 | `ops` no vacio | `bad_request` |
| 2 | `ops.len() <= MAX_BATCH_OPS` (1000) | `bad_request` |
| 3 | **firma de cada op**: `ovaultd_crypto::verify_record(...)` contra `vaults.vault_pubkey` | `unauthorized` |
| 4 | **idempotencia: `operation_id` ya existe** (se consulta en `batches`, no en `revisions`) | devuelve el mismo `rev` |
| 5 | `size` de cada op con blob contra `vaults.max_file_bytes` (`NULL` -> `MAX_FILE_BYTES`) | `max_file_exceeded` |
| 6 | todo `blob_id` referenciado existe en `blobs (vault_id, hash)` | `not_found` |

**El 3 va PRIMERO de todo lo que toca la base, y es lo unico que frena un log envenenado.** El log es append-only con
CAS: una revision con firma invalida que entre queda para siempre, los clientes la bajan, la verifican
(protocol.md:110) y la **rechazan**, y no pueden saltearla porque el `rev` ya avanzo. Un cliente roto dejaria el vault
inservible para todos. El servidor es el unico que ve la escritura **antes** de que sea irreversible, asi que la firma
se verifica **antes de escribir una fila**: si un op no verifica, no se toca `batches`, ni `revisions`, ni el CAS. Un
cliente que no puede firmar tampoco consulta la tabla de idempotencia.

**De donde sale cada campo firmado** (es el error mas facil de cometer, y ya se cometio dos veces): `path_id`,
`blob_id`, `t`, `op`, `size`, `mtime` y `operation_id(16)` salen **del op**; **`expected_rev` sale del sobre del
batch** (`Batch`) —hay uno solo por batch, y es el mismo que usa el CAS—; y `epoch` sale **de la fila del
vault**. El orden de los argumentos es el de `ovaultd_crypto::verify_record`, no el que uno recuerde: se lee de su
firma.

**El 4 va ANTES del CAS, y no es un detalle.** Repetir un batch que ya entro tiene que devolver el mismo `rev`
**aunque el head haya avanzado**: si el CAS fuera primero, un reintento despues de un ack perdido daria `conflict` en
vez del ack, y el cliente rebasaria una base que no cambio. Es el invariante de ADR-0003 y el test
`retry_after_lost_ack_returns_same_revision` lo mide.

### La transaccion

1. `SELECT head_rev FROM vaults WHERE id = ?` -> si `expected_rev != head_rev`:
   `Conflict { path_id: <el de la PRIMERA op>, expected: expected_rev, head: head_rev }`. El CAS es del batch entero.
2. `rev = head_rev + 1`.
3. Por cada op, **dentro de la misma transaccion**:
   - `del`: `files` -> `deleted = 1`, `rev = rev`, `blob_hash`/`t_mac`/`path_enc` en `NULL`; fila en `revisions` con
     `op='del'` y la `sig` del cliente.
   - `add`/`mod`: upsert de `files` (`path_enc`, `is_folder`, `blob_hash`, `t_mac`, `size`, `mtime`, `ctime`,
     `deleted = 0`, `rev = rev`) + fila en `revisions`.
   - `move`: upsert del path **nuevo** + el path viejo (`prev_path_id`) pasa a `deleted = 1` con `rev = rev`;
     **una** fila en `revisions` (`op='move'`, `path_id` = nuevo, `prev_path_id` = viejo). Si `prev_path_id` no existe
     en `files` -> `not_found`. **Sin filas derivadas.**
4. `blobs.refs`: `+1` por cada fila nueva de `revisions` con ese `blob_hash` (el invariante de `refs` cuenta
   `files` vivos + `revisions` vivas + `snapshots.manifest`, y esta transaccion agrega una revision por op).
5. Fila en `batches` (`vault_id`, `rev`, `batch_id`, `op_count`, `device_id`).
6. `UPDATE vaults SET head_rev = rev WHERE id = ?`.
7. `commit()`.

### Invariantes

- **Todas las filas comparten `rev`.** Un batch entra entero o no entra.
- **El servidor NUNCA inventa una fila de `revisions`.** Cada fila corresponde a una op del cliente y lleva **su**
  `sig`. Por eso un `move` no genera una fila `del` derivada: seria una fila sin firma, y el cliente rechaza historia
  sin firmar (anti-rollback).
- `expected_rev` se persiste en cada fila (ADR-0006): sin eso la firma no se puede **re-verificar** despues del push.
- `epoch` = el de `vaults.epoch` al momento del commit.
- **Cero plaintext:** el servidor guarda ciphertext, hashes y metadata. Ni un nombre ni un byte de contenido.

### Tests que este archivo tiene que dejar en verde

`concurrent_expected_rev_has_one_winner` · `retry_after_lost_ack_returns_same_revision` ·
`un_batch_de_n_ops_es_una_sola_revision` · `batch_con_operation_id_repetido_devuelve_la_misma_rev` ·
`move_marca_el_path_viejo_y_no_inventa_filas` · `move_de_un_path_inexistente_da_not_found` ·
`batch_sobre_max_batch_ops_da_bad_request` · `blob_inexistente_da_not_found`

## Invariantes que el gate va a exigir

1. **ACK después del commit**, nunca antes: si la transacción no commiteó, no hay ACK.
2. **Idempotencia real por `operation_id`** (`ADR-0003`): un reintento devuelve la MISMA revisión, no una nueva.
   El mismo `operation_id` con otro contenido se rechaza. Dedupe de blobs NO es idempotencia.
   La consulta va contra **`batches`**: `SELECT rev FROM batches WHERE vault_id = ? AND operation_id = ? LIMIT 1`, que
   es donde vive la clave única (`idx_batches_operation`). En **`revisions`** el `operation_id` va en **cada fila**
   (es un campo **FIRMADO**: `msg_rec` cubre `operation_id(16)`) y **no es único**: las N ops de un batch comparten
   uno solo. Si se guardara en una sola fila, el cliente **no podría re-verificar** la firma de las demás — y la
   verificación no puede depender de cuál fila es.
   **Carrera** de dos commits con el mismo `operation_id`: el que pierde choca con `idx_batches_operation` dentro de
   la transacción -> rollback, re-lectura de `batches` y respuesta idempotente. Un reintento legítimo **nunca** puede
   terminar en error de base.
3. **`expected_rev` es compare-and-swap**: dos escritores sobre la misma base ⇒ uno gana, el otro recibe el
   **MENSAJE** `conflict` con 409, no un `{"t":"error"}`: perder la carrera es un resultado normal, no un fallo.
4. **`refs` de `blobs` = files vivos + revisions vivas + `snapshots.manifest`**; se ajusta en la MISMA transacción.
5. **Un batch = una revisión**: `rename + edit` no se parte nunca en dos revisiones.
6. **Cross-vault siempre negado**: un blob de otro vault no se lee ni conociendo el hash.
7. **Cero plaintext en el server**: ni en SQLite, ni en WAL, ni en el CAS, ni en temporales, ni en logs.
8. Los `u64` del protocolo se guardan como `INTEGER` (SQLite i64): convertir con `u64::try_from` y **nunca** con `as`.

## Lo que NO hay que hacer

- No agregar dependencias (ya están todas en `Cargo.toml`).
- No usar PostgreSQL ni `sqlx::query` sin tipar.
- No inventar columnas: el esquema es `migrations/001_init.sql`.
- No dejar `todo!()` / `unimplemented!()` en el entregable.


---

## Delta — `sync/delta.rs`

```rust
pub async fn changes(pool: &SqlitePool, vault_id: i64, since: i64, limit: i64) -> Result<Changes>;
```

Sale de `docs/protocol.md` (Control) y `docs/implementation-map.md` §5.

1. El vault primero: `SELECT head_rev, min_rev FROM vaults WHERE id = ?`; sin fila -> `NotFound`.
2. **Piso de retención:** si `since < min_rev` -> `Changes { items: vec![], next_since: since, min_rev: Some(min_rev) }`.
   El cliente queda fuera de ventana y hace **resync completo**. Nunca una delta silenciosa: jamás devolver un pedazo de
   historia como si estuviera completo.
3. **La página va por revisión completa:** los registros con `rev > since`, ordenados por `(rev, path_id)`, acumulando
   revisiones **enteras** mientras entren en `limit`. Una revisión no se corta nunca.
   **Progreso garantizado:** si la PRIMERA revisión después de `since` tiene más registros que `limit`, se devuelve
   entera igual. Si no, un batch grande deja al cliente pidiendo lo mismo para siempre (deadlock por diseño).
4. `next_since` = la rev de la última revisión **completa** incluida; sin items, `next_since = since`.
   `min_rev` = `Some(..)` sólo en el caso 2; en una página normal va `None`.
5. **Mapeo fila -> `SignedRecord`, traduciendo los NULL a lo que el cliente FIRMÓ** (los bytes canónicos son de ancho
   fijo: no hay opcionales):

   | Columna NULL | En el `SignedRecord` | Por qué |
   |---|---|---|
   | `blob_hash` | `BlobId::new([0; 32])` | el cliente firmó 32 bytes cero en ese slot |
   | `t_mac` | base64 de **64 bytes cero** | idem: el slot del MAC son 64 bytes, no la cadena vacía |
   | `size`, `mtime` | `0` | `u64be(0)` |

   Si el delta devolviera `null` o vacío, el cliente **no podría re-verificar** la firma de ningún `del` ni de ninguna
   carpeta: la verificación fallaría en registros válidos.
6. `op` sale del CHECK (`'add'|'mod'|'del'|'move'` -> `Op`), `path_enc` de la columna BLOB (bytes; el wire los manda en
   base64 por el atributo de serde de la struct), `operation_id` textual, `epoch`, `prev_path_id` opcional.
7. Lectura: **una sola consulta**, `FROM revisions WHERE vault_id = ? AND rev > ? ORDER BY rev, path_id`. Nada de N+1.

### Tests nombrados (en el archivo, `#[cfg(test)] mod tests`)

Base: `crate::db::connect` + `sqlx::migrate!("../../migrations")`, como en `commit.rs`.

- `una_revision_no_se_corta_al_medio`
- `next_since_es_el_final_de_una_revision_completa`
- `since_menor_que_min_rev_pide_resync_completo`
- `una_revision_mas_grande_que_limit_igual_progresa`
- `sin_cambios_devuelve_pagina_vacia_y_el_mismo_since`
- `del_mapea_blob_y_tmac_en_cero_como_lo_que_se_firmo`
- `la_pagina_respeta_el_orden_rev_path_id`
- `vault_inexistente_da_not_found`


---

## Autenticación de cuenta — `auth.rs` (ADR-0011)

```rust
pub async fn crear_cuenta(pool: &SqlitePool, email: &str, password: &str) -> Result<Cuenta>;
pub async fn iniciar_sesion(pool: &SqlitePool, email: &str, password: &str, name: &str, platform: Option<&str>)
    -> Result<(String, i64)>;              // (token, device_id)
pub fn hash_password(password: &str) -> Result<String>;          // string PHC: params + salt adentro
pub fn verificar_password(password: &str, phc: &str) -> Result<bool>;
```

1. **argon2id** con los defaults de la crate (`Argon2::default()`): `m=19456 KiB`, `t=2`, `p=1`. Ojo: son los parámetros
   de la **password de cuenta**; los de la KDF del vault son otros y viven en `vaults.kdf_params` (ADR-0011: son
   conceptos independientes y **no se tocan entre sí**).
2. `crear_cuenta` hashea y guarda. `accounts.password_hash` es `NOT NULL`: sin password no hay cuenta. La creación de
   cuentas es **bootstrap local**, no un endpoint público (build-plan §Fase 1).
3. `iniciar_sesion`: busca por email y **siempre verifica contra un hash**, exista o no la cuenta (si no existe, contra un
   hash señuelo precalculado). Cualquier fallo -> **el mismo** `Unauthorized`, sin distinguir "no existe" de "password
   mal": distinguirlos convierte el login en un buscador de emails. Si verifica -> `emitir_device` y devuelve
   `(token, device_id)`.
4. `iniciar_sesion` **nunca** devuelve el hash, ni en logs, ni en mensajes de error.
5. MFA: no se implementa en este bloque. Si el transporte recibe `mfa`, responde `not_implemented` (ADR-0011 §5): **no se
   ignora en silencio**.
6. `hash_password`/`verificar_password` son `pub` para poder probarlos sin HTTP, y `verificar_password` devuelve `Ok(false)`
   —no error— cuando el hash no parsea o no verifica: un PHC corrupto en la base es un "no verifica", no un 500.

### Tests nombrados (en `auth.rs`, junto a los 4 que ya están)

- `password_correcta_da_token_y_device_id`
- `password_incorrecta_da_unauthorized`
- `email_inexistente_da_el_mismo_error_que_password_incorrecta`
- `el_hash_guardado_no_contiene_la_password_en_claro`
- `dos_hashes_de_la_misma_password_son_distintos` (salt aleatorio: si no, dos cuentas con la misma password comparten hash)


---

## Superficie HTTP — `routes.rs` (Fase 1, ADR-0011)

### Estado

```rust
#[derive(Clone)]
pub struct EstadoServidor { pub pool: SqlitePool, pub config: Config }
```
Todo handler recibe `State<EstadoServidor>` y devuelve **`Result<T>`** — el alias de `error.rs`
(`pub type Result<T> = std::result::Result<T, ServerError>`, error.rs:10) que ya trae el `ServerError` adentro.
`ServerError` **ya** implementa `IntoResponse` y la lista de códigos del protocolo es **cerrada**: no se agrega
una variante ni un código nuevo acá.

> Ojo con el trait del extractor: `from_request_parts` **no** usa el alias (ahí `Self::Rejection` es el error),
> así que su firma es `-> std::result::Result<Self, Self::Rejection>`. Con el alias importado, escribir
> `Result<Self, Self::Rejection>` son dos argumentos sobre un alias de uno: no compila.

### Endpoints de esta fase

| método | ruta | auth | qué hace |
|---|---|---|---|
| GET | `/` | no | raíz web: sin cuentas -> redirect a `/setup`; con cuentas -> redirect a `/admin` (panel, ADR-0017) |
| GET | `/setup` | no | **bootstrap del primer usuario** (ADR-0016): formulario SOLO mientras `accounts = 0`; con cuentas, 410 Gone; `OVAULTD_SETUP_TOKEN` (opcional) la condiciona con `?token=…`; la cuenta creada nace `admin` |
| POST | `/setup` | no | crea la primera cuenta (form: `email`, `password`, `token?`); 410 con cuentas existentes; 403 sin token cuando está configurado; `INSERT` con `WHERE NOT EXISTS` (carrera-safe) |
| GET | `/admin` | cookie | **panel de administración** (ADR-0017): sin sesión -> redirect a `/admin/login`; con sesión, dashboard con contadores |
| GET | `/admin/login` | no | formulario de login del panel (cookie `ovaultd_session`: `HttpOnly`, `SameSite=Lax`; `Secure` automático con `X-Forwarded-Proto: https`, forzable con `OVAULTD_SECURE_COOKIES=on/off`) |
| POST | `/admin/login` | no | email+password de CUENTA (argon2id con señuelo) -> cookie de sesión (30 días); freno de 5 intentos/min por ip+email (429 + `Retry-After`); todo va a `audit_log` |
| POST | `/admin/logout` | sesión + CSRF | revoca la sesión y expira la cookie |
| GET/POST | `/admin/mi-cuenta` | sesión + CSRF | cambiar la password de la propia cuenta (actual + nueva ≥8); al cambiarla se revocan las demás sesiones del panel |
| GET | `/admin/assets/{admin.css,htmx.min.js}` | no | assets embebidos en el binario (sin CDNs; htmx es progresivo: el panel funciona sin JS) |
| GET | `/healthz` | no | liveness/readiness: `{"status":"ok"}` (503 si la base no responde) — healthcheck de compose |
| GET | `/metrics` | no | Prometheus mínimo de operación (`ovaultd_up`, `ovaultd_uptime_seconds`); sin secretos; restringir en el proxy |
| POST | `/api/v1/auth/signin` | no | `{email, password, mfa?}` -> `{token, device_id}` |
| POST | `/api/v1/auth/signout` | sí | revoca el device del token -> 204 |
| GET | `/api/v1/me` | sí | `{account_id, email, devices: [...]}` |
| GET | `/api/v1/vaults` | sí | `[{id, name, role, rev}]` (`role` = `"owner"` en esta fase) |
| POST | `/api/v1/vaults` | sí | material cripto del cliente -> `{id}` |
| POST | `/api/v1/vaults/{id}/access` | sí | `{proof}` (firma del challenge) -> `{host, rev}` |
| GET | `/api/v1/vaults/{id}/changes?since=&limit=` | sí | delta (delega en `delta::changes`) |
| PUT | `/api/v1/vaults/{id}/blobs/{hash_hex}` | sí | cuerpo crudo -> 201 nuevo / 200 ya estaba |
| GET | `/api/v1/vaults/{id}/blobs/{hash_hex}` | sí | bytes; con `Range` -> 206 |
| GET | `/api/v1/devices` | sí | lista de devices de la cuenta |
| POST | `/api/v1/devices` | sí | enrolar otro device -> `{device_id, token}` |
| DELETE | `/api/v1/devices/{id}` | sí | revocar -> 204 |
| GET | `/api/v1/vaults/{id}/history?path_id=&before=&limit=` | sí | historia de un archivo: oplog por `path_id` (hex), más nueva primero, con la cadena de renames (`prev_path_id`) resuelta por CTE recursivo; `{items, next_before, min_rev}` |
| GET | `/api/v1/vaults/{id}/snapshots` | sí | snapshots etiquetados del vault (más nuevo primero) |
| POST | `/api/v1/vaults/{id}/snapshots` | sí | `{rev?, label?}` (corte clampeado al head) -> 201 con `manifest` `{path_id: blob_id}` |
| GET | `/api/v1/vaults/{id}/snapshots/{snap}` | sí | detalle con el manifest completo |
| DELETE | `/api/v1/vaults/{id}/snapshots/{snap}` | sí | borra y DEVUELVE las referencias del manifest -> 204 (404 si no existe) |

La **historia** es el oplog de `revisions` (índice `idx_rev_path`): el server no descifra nada y el cliente resuelve
nombres con su `k_path`/`path_enc`. Los **snapshots** etiquetan un corte coherente en `rev` y su manifest cuenta como
referencia viva de blobs: el alta y el borrado ajustan `blobs.refs` en la MISMA transacción (el invariante que el GC
respeta).

**Fuera de esta fase (declarado, no implementar):** `restore`, `tree`, `members`, `leave`,
`config`. El WS tampoco va acá: va en `ws.rs`.

### Auth: extractor `Autenticado`

```rust
pub struct Autenticado { pub account_id: i64, pub device_id: i64 }

impl FromRequestParts<EstadoServidor> for Autenticado {
    type Rejection = ServerError;
    async fn from_request_parts(parts: &mut Parts, state: &EstadoServidor) -> Result<Self, Self::Rejection> { ... }
}
```

- Lee `Authorization: Bearer <token>`. Sin header, sin `Bearer`, o token que no verifica -> `Unauthorized` (401).
- `auth::verificar_token(pool, token)` devuelve el **device_id**; el `account_id` sale de `devices.account_id`. Si esa
  fila no está, `Unauthorized` (no `NotFound`: un token válido sin device es un token roto).
- `verificar_token` ya tiene que rechazar tokens revocados y vencidos. Si no lo hace, **es un bug de `auth.rs`**: se
  reporta, no se parchea en la ruta.
- Un token de device **no-owner** del vault no ve el vault (ver aislamiento).
- **Nunca** se loguea el token, ni entero ni recortado.

### Aislamiento: 404, no 403

Toda ruta de vault resuelve primero `SELECT ... FROM vaults WHERE id = ? AND account_id = ?`. Si no hay fila -> **404**.
Un 403 confirma que el vault existe y a quién no pertenece: es un oráculo de existencia. **Todo** acceso a `revisions`,
`batches` y blobs pasa por ese chequeo, sin excepción — no hay consulta "por id de blob" suelta.

### Blobs

**El cuerpo se lee con `axum::body::to_bytes(request.into_body(), limite)`** — no a mano con
`into_data_stream()` y `StreamExt::next`: eso necesita `futures`, que es una **dev-dependency** y no existe en la
lib. `to_bytes` lee incrementalmente y **corta cuando pasa el límite** (no bufferea de más) y no agrega deps.

- El hash va en el path en **hex minúscula, 64 caracteres** (`sha256` del contenido, el mismo que identifica el blob en
  el CAS). No b64: `+` y `/` en un path son un problema de escapado y la misma identidad ya viaja en b64 dentro del
  JSON del wire, donde no hay tal problema.
- `PUT`: se hashea el cuerpo y se compara con el del path. Si no coincide -> `Invalid` (400) y **no se escribe nada**
  (ni un archivo temporal huérfano). Si el blob ya está en el CAS -> 200 sin reescribir (el CAS es idempotente: el mismo
  contenido tiene la misma identidad). Si es nuevo -> 201.
- Cuerpo máximo: `vault.max_file_bytes` o, si es `NULL`, `ovaultd_protocol::MAX_FILE_BYTES` (32 MiB). Lo que se pase ->
  `ServerError::Protocol(ErrorCode::MaxFileExceeded)` (el código **de la lista cerrada**, no `Invalid`: el protocolo ya
  tiene el suyo) con el tope aplicado **durante** la lectura, no después de bufferear.
- `GET`: sin `Range` -> 200 con el cuerpo completo; con `Range: bytes=a-b` -> 206 + `Content-Range`. Un `Range` que no
  parsea o fuera de rango -> 416.
- **El cuerpo es el blob ALMACENADO**: agrega el sobre `nonce[24] ‖ tag[16] = 40 B` al contenido (ADR-0014), así que
  el tope del cuerpo es el tope de contenido + 40.
- `Content-Type: application/octet-stream` en ambos.

### Subidas reanudables (`tus` core, ADR-0012)

El camino HTTP para blobs grandes es un **subset de tus 1.0** (creacion + core; sin las extensiones de
checksum/termination). El umbral de 1 MB es perilla del **cliente** (ADR-0004): el server no impone un
tamano minimo. `Tus-Resumable: 1.0.0` es obligatorio en las tres rutas; si falta -> `Invalid` (400).

- `POST /vaults/{id}/blobs` — crea la subida. Headers: `Upload-Length: <total>` (bytes) y
  `Upload-Metadata: blob_id <base64>` (misma forma que el `blob_head` del WS: 32 B). Respuesta 201 con
  `Location: /api/v1/vaults/{id}/uploads/{upload_id}`, `Upload-Offset`, `Upload-Length` y cuerpo
  `{upload_id, offset, total}`. El `total` se valida contra `min(vault.max_file_bytes, MAX_FILE_BYTES)`
  -> `max_file_exceeded` (413). Si el blob ya esta registrado para el vault, la subida **nace completa**
  (`offset == total`; no escribe bytes ni reserva cuota). Con cuota opt-in, la fila reserva el total en
  la MISMA sentencia (`INSERT … SELECT` con guard): dos creaciones simultaneas no pueden pasar las dos;
  si no entra -> `quota_exceeded` (413).
- `HEAD /vaults/{id}/uploads/{upload_id}` — donde reanudar: `Upload-Offset` y `Upload-Length`. 404 si no
  existe o ya vencio.
- `PATCH /vaults/{id}/uploads/{upload_id}` — sube un bloque: `Content-Type: application/offset+octet-stream`
  (si no -> 400) y `Upload-Offset: <n>`. Si `n` no es el offset confirmado -> **409** con `Upload-Offset`
  actual en el header y `bad_request` en el cuerpo ("un offset no sigue: hueco o repeticion"). Exito ->
  204 + `Upload-Offset` nuevo.
- **La longitud declarada no se cree**: un bloque que pase el `total` da 400 y **termina** esa subida
  (fila + temporal afuera: "subir de nuevo desde cero", nunca un truncado silencioso). El offset
  confirmado vive en la base (sobrevive reinicios) y el `.part` se alinea a el antes de escribir.
- Al completar, EN ESTE ORDEN: hash **real** del archivo == `blob_id` (si no -> 400 + limpieza) -> CAS
  (dos fases: temporal + `fsync` + rename) -> indice (`registrar_blob`) -> `offset = total`. Un fallo a
  mitad deja la subida reanudable, y el PATCH del bloque final reintentado es idempotente (204, sin
  reescribir nada).
- Vencimiento: TTL por subida (default 24 h, `OVAULTD_UPLOAD_TTL_HOURS`); `HEAD`/`PATCH` sobre una
  vencida dan 404 y el barrido (corre al crear una subida) borra fila + temporal y libera la reserva.
- La cuota tambien muerde en `PUT` y en el WS inline, via `storage::espacio_disponible` (sin reserva;
  la ventana de carrera chica esta aceptada en ADR-0012).

Tests nombrados (integracion, `tests/uploads.rs`):

- `upload_reanudable_completa_por_chunks_y_se_reanuda`
- `offset_con_hueco_da_409_y_devuelve_el_offset_actual`
- `retry_del_chunk_final_es_idempotente`
- `hash_final_que_no_coincide_da_400_y_no_publica`
- `exceso_sobre_lo_declarado_da_400_y_termina_la_subida`
- `cuota_se_reserva_al_crear_y_bloquea_a_la_segunda`
- `la_reserva_se_libera_al_vencer_la_subida`
- `put_respeta_la_cuota`
- `upload_de_otro_vault_da_404`

### `signin` y `mfa`

`mfa` presente y no nulo -> `ServerError::Invalid("mfa no implementado: no mandes el código, no se está verificando")`.
**No se ignora en silencio y no se emite token**: un login que dice OK sin revisar el segundo factor es una mentira de
seguridad, no una feature pendiente (ADR-0011 §5).

### `access`

- El `proof` se verifica **con `vaults.vault_pubkey`** (Ed25519) contra el challenge emitido por
  `auth::emitir_challenge` y consumido con `auth::consumir_challenge` (uso único y vencimiento: los maneja `auth.rs`).
- `host`: **el `Host` de la petición** (o `config.bind_addr` si no vino). Es lo único correcto detrás de un proxy, y no
  hay campo de URL pública en `Config` — si algún día hay multi-nodo, sale de ahí, no de un valor fijo.
- Respuesta: `{host, rev}` con el `rev` actual del vault (0 si nunca se escribió).

### Tests nombrados (en `routes.rs`, con `tower::ServiceExt::oneshot` sobre el `Router`)

- `signin_con_password_correcta_devuelve_token`
- `signin_con_mfa_da_invalid_y_no_emite_token`
- `sin_token_da_401` / `token_invalido_da_401` / `token_revocado_da_401`
- `vault_de_otra_cuenta_da_404_no_403`
- `crear_vault_devuelve_id_y_aparece_en_la_lista`
- `blob_put_y_get_devuelve_los_mismos_bytes`
- `blob_con_hash_que_no_coincide_da_400_y_no_guarda_nada`
- `blob_put_repetido_es_idempotente`
- `blob_get_con_range_devuelve_206_y_el_trozo`
- `changes_delega_en_delta_y_devuelve_los_items`
- `signout_revoca_el_token_y_despues_da_401`
- `healthz_responde_ok` / `metrics_expone_solo_operacion`


---

## Prueba de posesión de la clave del vault — `ovaultd-crypto` (la usan `/access` y el WS)

```rust
pub fn proof_msg(vault_id: i64, device_id: i64, challenge: &[u8]) -> Vec<u8>;
pub fn sign_proof(keys: &SubKeys, vault_id: i64, device_id: i64, challenge: &[u8]) -> [u8; 64];
pub fn verify_proof(
    vault_pubkey: &[u8; 32],
    vault_id: i64,
    device_id: i64,
    challenge: &[u8],
    proof: &[u8; 64],
) -> Result<()>;
```

1. El mensaje firmado es `challenge ‖ u64be(vault_id) ‖ u64be(device_id)`, y **vive en `proof_msg` y en ningún otro
   lado**: el cliente y el servidor tienen que firmar/verificar bytes idénticos, y si el layout se escribe dos veces, un
   día se cambia en uno solo. Los dos enteros van **big-endian y al final**, así el largo del challenge no vuelve
   ambigua la concatenación (con los enteros fijos al final, no hay dos pares (challenge, ids) que den los mismos
   bytes).
2. `vault_pubkey` es **la misma** que verifica los registros (`vaults.vault_pubkey`): quien puede firmar un registro
   puede probar posesión, y quien no puede firmar un registro no puede probar nada. No hay una segunda clave.
3. `verify_proof` devuelve `Ok(())` o `Err(...)`, **nunca un `bool`**: un booleano que alguien olvida mirar es la forma
   clásica de que una verificación de firma termine verificando nada.

### Tests nombrados (en `ovaultd-crypto`)

- `la_firma_de_prueba_verifica_con_su_publica`
- `el_mismo_challenge_con_otro_vault_id_no_verifica`
- `el_mismo_challenge_con_otro_device_id_no_verifica`
- `otro_challenge_no_verifica`
- `una_firma_de_otra_clave_no_verifica`
- `el_layout_es_el_documentado` — los bytes exactos de `proof_msg` (largo y contenido), para que cambiarlo rompa algo
  en vez de romper clientes en silencio.

---

## Leer el challenge pendiente — `auth.rs`

```rust
pub async fn challenge_pendiente(pool: &SqlitePool, device_id: i64, vault_id: i64) -> Result<Option<Vec<u8>>>;
```

El cliente manda **sólo la firma**, así que el servidor tiene que recuperar con qué challenge fue. Devuelve el más
reciente **sin usar y sin vencer** de ese (device, vault), o `None`. El consumo sigue siendo `consumir_challenge`, y ahí
está la garantía de uso único: **verificar y después consumir**; si dos pedidos concurrentes verifican el mismo
challenge, sólo uno gana el `UPDATE` y el otro recibe 401. Consumir primero y verificar después quema el challenge con
una firma inválida.


## Superficie WebSocket (`GET /api/v1/ws`) — lo que implementa el servidor

El protocolo (docs/protocol.md, "Datos") define los mensajes. Acá va lo que el servidor tiene que garantizar. Es una capa
**delgada**: no reimplementa nada de lo que ya existe — verifica con `ovaultd_crypto::verify_proof`, escribe con
`sync::commit::commit_batch` y resuelve el vault como el HTTP. Lo propio es la **maquina de estados** y el framing.

### 1. La maquina de estados (y el orden no es negociable)

```
Conectado  --hello-->  Saludado  --auth(proof)-->  Autenticado  --(batch/blob/ping)--> ...
```

- **Antes de `hello`**: cualquier mensaje que no sea `hello` -> `error` y **se cierra la conexion**. Un socket abierto no
  es un cliente: sin `hello` no hay vault, y sin vault no hay clave publica contra la cual verificar nada.
- **Antes de `auth`**: `batch`, `pull` o frames binarios -> `error` + cierre. El `hello` **prueba que el device dice quien
  dice**, no que pueda escribir.
- **`hello` con un vault que no es de la cuenta del device** -> `error` (mismo criterio que el HTTP: 404, nunca 403: no se
  confirma la existencia de un vault ajeno).
- **`proto` distinto de 1** -> `error` con detalle; la negociacion no es de seguridad, pero un cliente que habla otro
  protocolo no puede seguir.

### 2. El challenge: se consume DESPUES de verificar

`hello_ok` trae un challenge de 32 B (el mismo `auth::emitir_challenge` del endpoint `/access`). El `auth` trae
`proof = sign_proof(k_sign, challenge, vault_id, device_id)` y el server verifica con
`ovaultd_crypto::verify_proof(vault_pubkey, vault_id, device_id, challenge, proof)` — **ese es el orden real de sus
argumentos y se lee de su firma** (`crates/ovaultd-crypto/src/lib.rs`): primero los ids, despues el challenge, al final
el proof. Escribirlo al reves no compila, pero *casi* compila si los tipos coinciden — por eso se cita la firma..

**Regla, y es la misma que en `/access`**: el challenge se consume **solo si el proof verifica**. Un proof invalido deja el
challenge **vivo** para que el device legitimo pueda usarlo. Lo que se cierra por un proof invalido es **la conexion**, no
el challenge. (Si el challenge se consumiera antes, cualquiera con un socket y basura podria quemarle el challenge a un
device legitimo: es el mismo agujero del log envenenado, en el handshake.)

`auth_ok` devuelve `rev`: **la revision desde la que el server va a mandar `notify`** (la `since` pedida, acotada a la
cabeza real del vault).

### 2.b Quien es el device, y quien prueba tener la clave (dos preguntas distintas)

El WS exige **las dos cosas**, porque responden a preguntas diferentes:

- **Identidad de cuenta y device**: el bearer token del HTTP (la misma resolucion que el extractor `Autenticado`,
  factorizada en `routes::autenticar_token`). Un `WebSocketUpgrade` **no ejecuta extractores solo**, asi que el handler
  autentica el upgrade el mismo y guarda `account_id`/`device_id` por conexion. El token llega por `Authorization:
  Bearer` o por el subprotocolo `ovaultd.bearer.<token>` (clientes de navegador que no pueden fijar headers; **ADR-0015**)
  — el server negocia/ecoa `ovaultd.v1` y **nunca** el bearer.
- **Posesion de la clave del vault**: el `auth` con el proof. El token dice *quien sos*; el proof dice *que tenes la
  clave*. Un token robado no alcanza para leer ni escribir el contenido.

### 3. Framing binario (ADR-0004: un solo camino por blob)

- Frame binario = `transfer_id (8 B BE) | offset (8 B BE) | bytes`. El `blob_head` (JSON) lo precede y dice `transfer`, **`blob_id`** (b64: sin el, el receptor no puede verificar que los bytes hashean a lo que el emisor dice),
  `len` y `offset`.
- **`<= MAX_INLINE_BLOB_BYTES` (1 MB)**: por WS. Al completarse los bytes -> al CAS y `ack`.
**`> 1 MB`**: por WS **se rechaza** con `error` `max_file_exceeded`, y **el detalle del error dice** que use
`POST /api/v1/vaults/{id}/blobs` (reanudable, `Content-Range`). El servidor **no redirige**: un WebSocket no puede
redirigir la peticion original — **le dice al cliente donde ir y el cliente decide**. **Nunca los dos caminos para
el mismo blob.**
- Un frame cuya `transfer_id` no tiene `blob_head` abierto -> `error`. Un `offset` que no sigue (hueco o repeticion) ->
  `error` (el offset explicito es el que permite reanudar sin reenviar todo).

### 4. Lo que NO esta en esta capa

- **La logica de escritura**: `batch` se delega en `sync::commit::commit_batch` **tal cual** (misma verificacion de firma,
  misma idempotencia, misma transaccion). Si un `batch` por WS se escribiera por otro camino, la regla "la firma se
  verifica antes de tocar la base" tendria una puerta de atras.
- **El fan-out de `notify` a otros devices** del vault: fase posterior. En esta, `notify` sale del stream de revisiones y
  el servidor manda lo que corresponde al cliente conectado.

### 4.b Un solo camino, y UN SOLO router

`crate::routes::router(estado)` monta **todo**: las rutas HTTP y tambien `GET /api/v1/ws`. No hay un router aparte para
el WS ni un segundo lugar donde montarlo: **si hubiera dos formas de montar, una seria la equivocada** (es la misma
leccion que el handler duplicado, un nivel mas arriba). `main.rs` sirve ese router y nada mas.

El WebSocket **no** se agrega con `.route(..)` despues: `router(estado)` devuelve el router **con el estado ya aplicado**
(`Router<()>`), asi que una ruta cuyo handler todavia necesita `State<EstadoServidor>` no entra. Va adentro, junto a las
demas.


El handler del WS **es uno**: el que recibe el extractor `Autenticado` y fija `account_id`/`device_id` por conexion. Un
segundo handler publico que haga el upgrade **sin** el extractor no es "una variante": es **la puerta de atras** de la
regla de identidad, y basta con montar el equivocado para perderla sin que nada falle. Si existe, se borra.

### 5. Los codigos de error

El WS usa **la misma lista cerrada** que el HTTP, en `snake_case` en el cable (`max_file_exceeded`, `not_found`,
`conflict`, ...). Es una lista cerrada: no se inventan codigos. `{"t":"error","code":"...","detail":"..."}`, y `detail` es
para humanos (no para ramificar logica).

### 6. Tests (nombres exactos; van en `src/ws/tests.rs`)

1.  `sin_hello_cualquier_mensaje_da_error_y_cierra`
2.  `hello_devuelve_challenge_y_head_rev`
3.  `hello_con_vault_ajeno_da_error_y_cierra`
4.  `hello_con_proto_distinto_da_error`
5.  `batch_antes_de_auth_da_error_y_cierra`
6.  `auth_con_proof_valido_da_auth_ok_con_la_rev_pedida`
7.  `auth_con_proof_invalido_da_error_y_no_quema_el_challenge`  <- el que importa
8.  `un_segundo_auth_con_el_mismo_challenge_da_error`
9.  `batch_valido_da_batch_ack_con_la_rev_y_una_sola_revision`
10. `batch_con_firma_invalida_da_error_y_no_escribe_nada`  (count(*) = 0 en revisions y batches)
11. `batch_con_operation_id_repetido_da_el_mismo_ack`
12. `blob_inline_chico_termina_en_el_cas_y_da_ack`
13. `blob_inline_mayor_al_umbral_da_max_file_exceeded_y_manda_al_http`
14. `frame_sin_blob_head_da_error`
15. `offset_con_hueco_da_error`
16. `mensaje_que_no_es_json_da_error_sin_tumbar_la_conexion`
17. `el_error_del_ws_usa_los_mismos_codigos_que_el_http_en_snake_case`
18. `sin_token_no_hay_handshake`  (sin el bearer del HTTP no se abre el socket: la identidad la da el token, no el `hello`)

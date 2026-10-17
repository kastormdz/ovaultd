-- 001_init.sql - esquema inicial, NIVEL A (SQLite con WAL)
--
-- Nivel A = 1 contenedor + SQLite WAL + CAS local. Es el default y lo unico a implementar.
-- El nivel B (PostgreSQL) usa el MISMO esquema con estos cambios de tipo:
--   INTEGER PRIMARY KEY AUTOINCREMENT -> BIGSERIAL / GENERATED
--   BLOB -> BYTEA . TEXT -> TIMESTAMPTZ / JSONB . INTEGER -> BIGINT donde corresponda
-- No se mantienen dos migraciones a mano: se genera la del backend elegido desde esta fuente.
--
-- Convenciones:
--   * Todo id de contenido es BYTEA/BLOB de 32 B (path_id, blob_id, t_mac, sig).
--   * `content_hash` del cliente NO se persiste: es local (deteccion de cambios sobre plaintext).
--   * `blob_id` = SHA-256 del CIPHERTEXT: clave del CAS.

-- Los PRAGMA NO van aca: `sqlx::migrate!` corre cada migracion DENTRO de una transaccion y SQLite rechaza
-- `PRAGMA synchronous` ahi adentro ("Safety level may not be changed inside a transaction"). Ademas son propiedades
-- de la CONEXION, no del esquema: los aplica `db::connect` (journal_mode WAL, foreign_keys, busy_timeout,
-- synchronous NORMAL) en cada conexion del pool. La migracion solo define el esquema.

-- ------------------------------------------------------------------ cuentas
CREATE TABLE accounts (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  email       TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,           -- argon2id (string PHC: parametros y salt adentro). Es la password DE
                                           -- CUENTA: independiente de la E2E que deriva la KEK (ADR-0011).
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),
  deleted_at  TEXT
);

CREATE TABLE devices (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  name        TEXT NOT NULL,
  platform    TEXT,
    -- El hash del token NO vive aca: vive en device_tokens, que es la unica fuente de verdad y ademas
    -- guarda expires_at y revoked_at por token (rotacion y revocacion individuales). Esta columna estaba
    -- con NOT NULL y contradecia a device_tokens.
    -- (corregido 2026-09-25, antes de cualquier despliegue: no hay base creada, no hace falta una 003)
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),
  last_seen   TEXT,
  revoked_at  TEXT
);
CREATE INDEX idx_devices_account ON devices(account_id);

-- ------------------------------------------------------------------- vaults
CREATE TABLE vaults (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  account_id     INTEGER NOT NULL REFERENCES accounts(id),
  name           TEXT NOT NULL,
  enc_version    INTEGER NOT NULL DEFAULT 2,     -- 2 = sync2; 1 = sync1 historico
  salt           BLOB NOT NULL,
  kdf_alg        TEXT NOT NULL DEFAULT 'argon2id',
  kdf_params     TEXT NOT NULL,                  -- JSON EN TEXTO: m, t, p, dkLen (versionado por vault)
  wrapped_key_pw BLOB NOT NULL,                  -- vault_key envuelta por el KEK de la password
  wrapped_key_rc BLOB NOT NULL,                  -- vault_key envuelta por la recovery
  vault_pubkey   BLOB NOT NULL,                  -- Ed25519 publica: el server verifica firmas y NO guarda secretos
  verifier       BLOB,                           -- opcional: verificacion previa sin credencial (no es un proof)
  epoch          INTEGER NOT NULL DEFAULT 1,     -- rotacion de vault_key (distinta de rotar la password)
  quota_bytes    INTEGER,                        -- NULL = ilimitado (opt-in, es POLITICA)
  max_file_bytes INTEGER,                        -- NULL = usar MAX_FILE_BYTES del protocolo (32 MiB).
                                                 -- NUNCA puede SUPERAR el techo: es seguridad de
                                                 -- memoria, no politica (ADR-0007, costo O(~4n))
  head_rev       INTEGER NOT NULL DEFAULT 0,
  min_rev        INTEGER NOT NULL DEFAULT 0,     -- piso de retencion: `since` menor => resync completo
  created_at     TEXT NOT NULL DEFAULT (datetime('now')),
  deleted_at     TEXT
);
-- Sin `region`: single-region por diseno (ADR-0005). Sin tiers de cuota: opt-in.

CREATE TABLE vault_members (
  vault_id   INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  role       TEXT NOT NULL CHECK (role IN ('owner','editor','reader')),
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (vault_id, account_id)
);

-- ------------------------------------------------------ estado actual (indice)
CREATE TABLE files (
  vault_id   INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
  path_id    BLOB NOT NULL,              -- HMAC(k_path, path) - determinista
  path_enc   BLOB,                       -- nombre cifrado (AAD = path_id); NULL en tombstones
  is_folder  INTEGER NOT NULL DEFAULT 0,
  blob_hash  BLOB,                       -- = blob_id (CAS sobre ciphertext). NULL en carpetas y tombstones
  t_mac      BLOB,                       -- firma de binding path_id <-> blob_id (ADR-0002)
  size       INTEGER,
  mtime      INTEGER,
  ctime      INTEGER,
  rev        INTEGER NOT NULL,
  deleted    INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (vault_id, path_id)
);
CREATE INDEX idx_files_vault_rev ON files(vault_id, rev);
CREATE INDEX idx_files_blob      ON files(vault_id, blob_hash);

-- ------------------------------------------------------------- historia (oplog)
CREATE TABLE batches (
  vault_id   INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
  rev        INTEGER NOT NULL,
  batch_id   TEXT NOT NULL,              -- UUID: agrupa los cambios atomicos de ESTA revision
    operation_id TEXT NOT NULL,            -- idempotencia real (ADR-0003): UNICA por vault
  op_count   INTEGER NOT NULL,
  device_id  INTEGER REFERENCES devices(id),
  ts         TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (vault_id, rev)
);
  -- La clave de idempotencia es el BATCH, no cada registro: un reintento (mismo operation_id)
  -- tiene que devolver la MISMA revision sin escribir nada (ADR-0003).
  CREATE UNIQUE INDEX idx_batches_operation ON batches(vault_id, operation_id);

CREATE TABLE revisions (
  vault_id     INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
  rev          INTEGER NOT NULL,
  path_id      BLOB NOT NULL,
  op           TEXT NOT NULL CHECK (op IN ('add','mod','del','move')),
  path_enc     BLOB,                     -- necesario para historia y bootstrap (un device nuevo no deriva nombres de HMACs)
  prev_path_id BLOB,                     -- metadata historica de renames
  blob_hash    BLOB,                     -- NULL en 'del' y carpetas
  t_mac        BLOB,
  sig          BLOB,                     -- firma Ed25519 del registro (autentica tambien 'del')
  epoch        INTEGER NOT NULL DEFAULT 1,
  size         INTEGER,
  mtime        INTEGER,
  device_id    INTEGER REFERENCES devices(id),
  operation_id TEXT NOT NULL,            -- va en CADA fila: es un campo FIRMADO (msg_rec cubre
                                         -- operation_id(16)) y ADR-0008 hace que todas las ops de un
                                         -- batch compartan el mismo. La unicidad por operacion NO se
                                         -- controla aca: se controla en `batches`.
  expected_rev INTEGER NOT NULL DEFAULT 0, -- base firmada por el cliente (cadena, ADR-0006). SE PERSISTE:
                                           -- sin esto la firma no se puede RE-verificar despues del push.
                                           -- 0 = primer registro del vault (sin base)
  ts           TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (vault_id, rev, path_id)
);
-- ojo: NO es UNIQUE. En ADR-0008 un batch es UNA revision y sus N ops comparten operation_id:
-- un indice unico aca hace IMPOSIBLE escribir un batch de mas de una operacion (y empuja a guardar
-- el id en una sola fila, que rompe la re-verificacion de la firma de las demas).
CREATE INDEX idx_rev_operation ON revisions(vault_id, operation_id);
CREATE INDEX idx_rev_path ON revisions(vault_id, path_id, rev);
CREATE INDEX idx_rev_blob ON revisions(vault_id, blob_hash);

-- ------------------------------------------------------------------------ CAS
CREATE TABLE blobs (
  vault_id   INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
  hash       BLOB NOT NULL,              -- blob_id
  size       INTEGER NOT NULL,
  refs       INTEGER NOT NULL DEFAULT 0, -- INVARIANTE: files + revisions vivas + snapshots.manifest
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (vault_id, hash)
);
CREATE INDEX idx_blobs_refs ON blobs(vault_id, refs);

-- ------------------------------------------------------------------ snapshots
CREATE TABLE snapshots (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  vault_id   INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
  rev        INTEGER NOT NULL,
  label      TEXT,
  manifest   TEXT NOT NULL,              -- JSON en texto: {path_id: blob_id} - CUENTA como referencia viva para el GC
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_snapshots_vault ON snapshots(vault_id, rev);

-- ------------------------------------------------------------- tokens activos
CREATE TABLE device_tokens (
  token_hash BLOB PRIMARY KEY,
  device_id  INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  issued_at  TEXT NOT NULL DEFAULT (datetime('now')),
  expires_at TEXT NOT NULL,
  revoked_at TEXT
);
CREATE INDEX idx_tokens_device ON device_tokens(device_id);

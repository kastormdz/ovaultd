-- 005: panel web de administración (ADR-0017).
--
-- Rol de cuenta: 'admin' opera el server desde el panel; 'user' es una cuenta común.
-- La cuenta del bootstrap por web (/setup, ADR-0016) nace 'admin'; en un servidor ya
-- bootstrapeado antes de esta migración, la más antigua pasa a 'admin' (es quien operó
-- el server hasta ahora).

ALTER TABLE accounts ADD COLUMN role TEXT NOT NULL DEFAULT 'user';
UPDATE accounts SET role = 'admin' WHERE id = (SELECT MIN(id) FROM accounts);

-- Sesiones WEB del panel: cookie opaca, separada de device_tokens (no sirven para el API
-- ni el WS). El token de sesión crudo solo vive en la cookie (acá queda su SHA-256);
-- el token CSRF se guarda crudo porque tiene que viajar en el HTML de cada formulario.
CREATE TABLE sessions (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  token_hash  BLOB NOT NULL UNIQUE,
  csrf_token  TEXT NOT NULL,
  account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),
  expires_at  TEXT NOT NULL,
  last_seen   TEXT,
  ip          TEXT,
  user_agent  TEXT
);
CREATE INDEX idx_sessions_account ON sessions(account_id);
CREATE INDEX idx_sessions_expira ON sessions(expires_at);

-- Auditoría: login (ok/fallido), altas/bajas de cuentas, revocaciones, ajustes…
-- El actor puede quedar NULL si la cuenta se borró después (ON DELETE SET NULL).
CREATE TABLE audit_log (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  ts               TEXT NOT NULL DEFAULT (datetime('now')),
  actor_account_id INTEGER REFERENCES accounts(id) ON DELETE SET NULL,
  action           TEXT NOT NULL,
  detail           TEXT,
  ip               TEXT
);
CREATE INDEX idx_audit_ts ON audit_log(ts);

-- Ajustes del server en key/value. El registro de cuentas nace CERRADO (ADR-0011):
-- abrirlo es una decisión explícita del admin desde el panel.
CREATE TABLE server_settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
INSERT INTO server_settings (key, value) VALUES ('registro_abierto', 'off');

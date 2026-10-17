-- 006: cuentas administradas desde el panel (W1).
--
-- Deshabilitar sin borrar: la cuenta no puede loguear (panel ni API/WS) y sus sesiones web
-- mueren; los datos quedan intactos y se puede rehabilitar.
ALTER TABLE accounts ADD COLUMN disabled_at TEXT;

-- Reset de password con token de UN uso (sin SMTP: el admin lo pasa a mano). Se guarda
-- solo el SHA-256 del token; el crudo viaja en el link una sola vez.
CREATE TABLE password_resets (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  token_hash BLOB NOT NULL UNIQUE,
  account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  expires_at TEXT NOT NULL,
  used_at    TEXT
);
CREATE INDEX idx_resets_account ON password_resets(account_id);

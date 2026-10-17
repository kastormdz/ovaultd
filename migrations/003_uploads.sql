-- Subidas reanudables (tus core, ADR-0012).
--
-- El offset confirmado vive ACÁ: sobrevive reinicios del server (el `.part` vive en
-- `cas_root/.uploads/<upload_id>.part` y se alinea a `offset` antes de cada escritura).
-- La reserva viva de cuota de una subida es `total - offset` (una subida completa reserva 0);
-- `expires_at` la vence y el barrido borra la fila + el temporal.
CREATE TABLE uploads (
  upload_id  TEXT PRIMARY KEY,
  vault_id   INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
  hash       BLOB NOT NULL,              -- blob_id declarado en Upload-Metadata (32 B)
  total      INTEGER NOT NULL,           -- Upload-Length declarado (no se cree: se verifica el real)
  offset     INTEGER NOT NULL DEFAULT 0, -- bytes confirmados (fsync + UPDATE en ese orden)
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  expires_at TEXT NOT NULL
);
CREATE INDEX idx_uploads_vault ON uploads(vault_id);
CREATE INDEX idx_uploads_expira ON uploads(expires_at);

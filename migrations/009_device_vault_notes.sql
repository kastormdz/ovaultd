-- Notas por (dispositivo, vault), reportadas por el CLIENTE.
--
-- La 008 guardó `devices.notes`: un solo número por device. Eso alcanza mientras un device
-- sincronice un vault, pero no tiene identidad de vault: si el mismo device sincroniza dos,
-- el número se acredita a ambos y el panel suma de más. Además, las vistas por vault tomaban
-- el `notes` del device de la última revisión, que puede ser el conteo de OTRO vault.
--
-- Esta tabla da la identidad que faltaba. `devices.notes` queda como último reporte del
-- device (sin vault) para que un rollback a la versión anterior siga mostrando algo.
CREATE TABLE device_vault_notes (
    device_id  INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    vault_id   INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    notes      INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (device_id, vault_id)
) WITHOUT ROWID;

CREATE INDEX idx_device_vault_notes_vault
    ON device_vault_notes (vault_id, updated_at DESC);

-- Backfill sólo de lo que no es ambiguo: devices que tocaron UN único vault. Los que
-- tocaron varios se llenan con el próximo `hello`, que ya trae el vault del handshake.
INSERT INTO device_vault_notes (device_id, vault_id, notes, updated_at)
SELECT d.id, MIN(r.vault_id), d.notes, CAST(strftime('%s', 'now') AS INTEGER)
  FROM devices d
  JOIN revisions r ON r.device_id = d.id
 WHERE d.notes IS NOT NULL
 GROUP BY d.id
HAVING COUNT(DISTINCT r.vault_id) = 1;

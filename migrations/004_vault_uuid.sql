-- ADR-0013: el UUID de 16 B que genera el CLIENTE (entra en los AAD de las envueltas y en
-- `msg_bind`) tiene que sobrevivir server-side, o un device nuevo no puede desbloquear nada.
--   - Opcional por retrocompatibilidad: las filas viejas quedan NULL y esos vaults siguen
--     funcionando donde su material local exista (no hay backfill posible).
--   - Índice único PARCIAL: NULL permitido, duplicados rechazados (ON CONFLICT DO NOTHING).
ALTER TABLE vaults ADD COLUMN vault_uuid BLOB;

CREATE UNIQUE INDEX idx_vaults_uuid
    ON vaults(vault_uuid)
    WHERE vault_uuid IS NOT NULL;

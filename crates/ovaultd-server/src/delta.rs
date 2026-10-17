use base64::{engine::general_purpose::STANDARD, Engine as _};
use sqlx::{Row, SqlitePool};
use ovaultd_protocol::{BlobId, Changes, Op, OperationId, PathId, SignedRecord};

use crate::error::{Result, ServerError};

fn invalid(detail: impl Into<String>) -> ServerError {
    ServerError::Invalid(detail.into())
}

fn decode_blob_id(value: Option<Vec<u8>>) -> Result<BlobId> {
    let bytes = value.unwrap_or_else(|| vec![0; 32]);
    BlobId::try_from(bytes).map_err(|error| invalid(error))
}

fn decode_path_id(value: Vec<u8>) -> Result<PathId> {
    PathId::try_from(value).map_err(|error| invalid(error))
}

fn decode_operation_id(value: Option<String>) -> Result<OperationId> {
    let Some(value) = value else {
        return Ok(OperationId::new([0; 16]));
    };

    // La MISMA forma del wire (`OperationId::from_wire`): el commit persiste el string tal
    // cual llega (UUID hex, guiones tolerados). Decodificarlo como base64 era un bug real:
    // cualquier delta con datos escritos por el commit respondía 400.
    OperationId::from_wire(&value)
        .map_err(|error| invalid(format!("operation_id inválido ({error}): {value}")))
}

fn decode_op(value: String) -> Result<Op> {
    match value.as_str() {
        "add" => Ok(Op::Add),
        "mod" => Ok(Op::Mod),
        "del" => Ok(Op::Del),
        "move" => Ok(Op::Move),
        other => Err(invalid(format!("operación inválida: {other}"))),
    }
}

fn checked_revision(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| invalid(format!("{field} fuera de rango")))
}

fn checked_epoch(value: i64) -> Result<u16> {
    u16::try_from(value).map_err(|_| invalid("epoch fuera de rango"))
}

pub async fn changes(
    pool: &SqlitePool,
    vault_id: i64,
    since: i64,
    limit: i64,
) -> Result<Changes> {
    let vault = sqlx::query(
        "SELECT head_rev, min_rev
         FROM vaults
         WHERE id = ?",
    )
    .bind(vault_id)
    .fetch_optional(pool)
    .await?
    .ok_or(ServerError::NotFound)?;

    let min_rev: i64 = vault.get("min_rev");

    if since < min_rev {
        return Ok(Changes {
            items: Vec::new(),
            next_since: checked_revision(since, "since")?,
            min_rev: Some(checked_revision(min_rev, "min_rev")?),
        });
    }

    let rows = sqlx::query(
        "SELECT rev, path_id, op, path_enc, prev_path_id,
                blob_hash, t_mac, sig, epoch, size, mtime,
                operation_id, expected_rev
         FROM revisions
         WHERE vault_id = ? AND rev > ?
         ORDER BY rev, path_id",
    )
    .bind(vault_id)
    .bind(since)
    .fetch_all(pool)
    .await?;

    let mut items = Vec::new();
    let mut next_since = since;
    let mut index = 0;

    while index < rows.len() {
        let revision = rows[index].get::<i64, _>("rev");
        let mut end = index + 1;

        while end < rows.len() && rows[end].get::<i64, _>("rev") == revision {
            end += 1;
        }

        let revision_len = end - index;
        let can_append = items.is_empty()
            || limit <= 0
            || (items.len() as i64 + revision_len as i64 <= limit);

        if !can_append {
            break;
        }

        for row in &rows[index..end] {
            let rev = row.get::<i64, _>("rev");
            let path_id = decode_path_id(row.get::<Vec<u8>, _>("path_id"))?;
            let op = decode_op(row.get::<String, _>("op"))?;

            let blob_id = decode_blob_id(row.get::<Option<Vec<u8>>, _>("blob_hash"))?;

            let t_mac = row
                .get::<Option<Vec<u8>>, _>("t_mac")
                .map(|value| STANDARD.encode(value))
                .unwrap_or_else(|| STANDARD.encode([0u8; 64]));

            let sig = STANDARD.encode(row.get::<Vec<u8>, _>("sig"));

            let path_enc = row
                .get::<Option<Vec<u8>>, _>("path_enc")
                .unwrap_or_default();

            let size = row.get::<Option<i64>, _>("size").unwrap_or(0);
            let mtime = row.get::<Option<i64>, _>("mtime").unwrap_or(0);

            let size = u64::try_from(size).map_err(|_| invalid("size fuera de rango"))?;
            let mtime = u64::try_from(mtime).map_err(|_| invalid("mtime fuera de rango"))?;

            let expected_rev =
                checked_revision(row.get::<i64, _>("expected_rev"), "expected_rev")?;

            let operation_id =
                decode_operation_id(row.get::<Option<String>, _>("operation_id"))?;

            let epoch = checked_epoch(row.get::<i64, _>("epoch"))?;

            let prev_path_id = row
                .get::<Option<Vec<u8>>, _>("prev_path_id")
                .map(decode_path_id)
                .transpose()?;

            items.push(SignedRecord {
                rev: checked_revision(rev, "rev")?,
                op,
                path_id,
                blob_id,
                path_enc,
                t_mac,
                sig,
                size,
                mtime,
                expected_rev,
                operation_id,
                epoch,
                prev_path_id,
            });
        }

        next_since = revision;
        index = end;
    }

    Ok(Changes {
        items,
        next_since: checked_revision(next_since, "next_since")?,
        min_rev: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;

    struct TestContext {
        pool: SqlitePool,
        path: PathBuf,
        vault_id: i64,
    }

    impl TestContext {
        async fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
            let path =
                std::env::temp_dir().join(format!("ovaultd-server-delta-{}.db", Uuid::new_v4()));
            let path_string = path.to_str().ok_or("ruta temporal inválida")?.to_owned();

            let pool = crate::db::connect(&path_string).await?;
            sqlx::migrate!("../../migrations").run(&pool).await?;

            let account_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO accounts (email, password_hash)
                 VALUES (?, ?)
                 RETURNING id",
            )
            .bind(format!("{}@example.com", Uuid::new_v4()))
            .bind(crate::auth::phc_de_prueba())
            .fetch_one(&pool)
            .await?;

            let vault_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO vaults
                    (account_id, name, salt, kdf_params,
                     wrapped_key_pw, wrapped_key_rc, vault_pubkey)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 RETURNING id",
            )
            .bind(account_id)
            .bind("test-vault")
            .bind(vec![0u8; 16])
            .bind("{}")
            .bind(vec![0u8; 32])
            .bind(vec![0u8; 32])
            .bind(vec![0u8; 32])
            .fetch_one(&pool)
            .await?;

            Ok(Self {
                pool,
                path,
                vault_id,
            })
        }

        async fn insert_revision(
            &self,
            rev: i64,
            path_value: u8,
            op: &str,
            operation_value: u8,
        ) -> std::result::Result<(), Box<dyn std::error::Error>> {
            let operation_id = uuid_de_prueba([operation_value; 16]);

            sqlx::query(
                "INSERT INTO revisions
                    (vault_id, rev, path_id, op, path_enc, prev_path_id,
                     blob_hash, t_mac, sig, epoch, size, mtime,
                     operation_id, expected_rev)
                 VALUES (?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(self.vault_id)
            .bind(rev)
            .bind(vec![path_value; 32])
            .bind(op)
            .bind(vec![path_value])
            .bind(vec![path_value; 32])
            .bind(vec![path_value; 64])
            .bind(vec![path_value; 64])
            .bind(1_i64)
            .bind(10_i64)
            .bind(20_i64)
            .bind(operation_id)
            .bind(rev - 1)
            .execute(&self.pool)
            .await?;

            Ok(())
        }

        async fn set_head_min(
            &self,
            head_rev: i64,
            min_rev: i64,
        ) -> std::result::Result<(), Box<dyn std::error::Error>> {
            sqlx::query(
                "UPDATE vaults
                 SET head_rev = ?, min_rev = ?
                 WHERE id = ?",
            )
            .bind(head_rev)
            .bind(min_rev)
            .bind(self.vault_id)
            .execute(&self.pool)
            .await?;

            Ok(())
        }
    }

    impl Drop for TestContext {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_file(self.path.with_extension("db-shm"));
            let _ = std::fs::remove_file(self.path.with_extension("db-wal"));
        }
    }

    /// La forma REAL que persiste el commit para `operation_id`: el string tal cual llega
    /// (UUID hex con guiones). El fixture viejo insertaba base64 y el decoder lo "probaba"
    /// contra datos que el escritor nunca produce — el bug llegó hasta el smoke.
    fn uuid_de_prueba(bytes: [u8; 16]) -> String {
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }

    async fn three_revisions(
        context: &TestContext,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        for rev in 1..=3 {
            for offset in 0..4 {
                context
                    .insert_revision(rev, (rev * 10 + offset) as u8, "add", rev as u8)
                    .await?;
            }
        }

        context.set_head_min(3, 0).await?;
        Ok(())
    }

    #[tokio::test]
    async fn una_revision_no_se_corta_al_medio(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;
        three_revisions(&context).await?;

        let result = changes(&context.pool, context.vault_id, 0, 6).await?;

        assert_eq!(result.items.len(), 4);
        assert!(result.items.iter().all(|item| item.rev == 1));

        Ok(())
    }

    #[tokio::test]
    async fn next_since_es_el_final_de_una_revision_completa(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;
        three_revisions(&context).await?;

        let result = changes(&context.pool, context.vault_id, 0, 6).await?;

        assert_eq!(result.next_since, 1);
        assert!(result.items.iter().all(|item| item.rev == 1));

        Ok(())
    }

    #[tokio::test]
    async fn since_menor_que_min_rev_pide_resync_completo(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;
        context.set_head_min(5, 5).await?;

        let result = changes(&context.pool, context.vault_id, 2, 10).await?;

        assert!(result.items.is_empty());
        assert_eq!(result.next_since, 2);
        assert_eq!(result.min_rev, Some(5));

        Ok(())
    }

    #[tokio::test]
    async fn una_revision_mas_grande_que_limit_igual_progresa(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;

        for path_value in 0..10 {
            context
                .insert_revision(1, path_value, "add", path_value)
                .await?;
        }

        context.set_head_min(1, 0).await?;

        let result = changes(&context.pool, context.vault_id, 0, 3).await?;

        assert_eq!(result.items.len(), 10);
        assert_eq!(result.next_since, 1);

        Ok(())
    }

    #[tokio::test]
    async fn sin_cambios_devuelve_pagina_vacia_y_el_mismo_since(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;
        context.set_head_min(3, 0).await?;

        let result = changes(&context.pool, context.vault_id, 3, 10).await?;

        assert!(result.items.is_empty());
        assert_eq!(result.next_since, 3);
        assert_eq!(result.min_rev, None);

        Ok(())
    }

    #[tokio::test]
    async fn del_mapea_blob_y_tmac_en_cero_como_lo_que_se_firmo(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;

        sqlx::query(
            "INSERT INTO revisions
                (vault_id, rev, path_id, op, path_enc, prev_path_id,
                 blob_hash, t_mac, sig, epoch, size, mtime,
                 operation_id, expected_rev)
             VALUES (?, 1, ?, 'del', NULL, NULL, NULL, NULL, ?, 1, NULL, NULL, ?, 0)",
        )
        .bind(context.vault_id)
        .bind(vec![1u8; 32])
        .bind(STANDARD.encode([3u8; 16]))
        .bind(uuid_de_prueba([3u8; 16]))
        .execute(&context.pool)
        .await?;

        context.set_head_min(1, 0).await?;

        let result = changes(&context.pool, context.vault_id, 0, 10).await?;
        let item = &result.items[0];

        assert_eq!(item.blob_id, BlobId::new([0; 32]));
        assert_eq!(item.t_mac, STANDARD.encode([0u8; 64]));
        assert_eq!(item.size, 0);
        assert_eq!(item.mtime, 0);

        Ok(())
    }

    #[tokio::test]
    async fn la_pagina_respeta_el_orden_rev_path_id(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;

        for path_value in [30u8, 10, 20] {
            context
                .insert_revision(1, path_value, "add", path_value)
                .await?;
        }

        context.set_head_min(1, 0).await?;

        let result = changes(&context.pool, context.vault_id, 0, 10).await?;

        let paths: Vec<u8> = result
            .items
            .iter()
            .map(|item| item.path_id.as_bytes()[0])
            .collect();

        assert_eq!(paths, vec![10, 20, 30]);

        Ok(())
    }

    #[tokio::test]
    async fn vault_inexistente_da_not_found(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;

        let result = changes(&context.pool, context.vault_id + 1, 0, 10).await;

        assert!(matches!(result, Err(ServerError::NotFound)));

        Ok(())
    }
}

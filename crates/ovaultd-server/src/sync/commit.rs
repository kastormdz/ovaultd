use base64::{engine::general_purpose::STANDARD, Engine as _};
use sqlx::{FromRow, Sqlite, SqlitePool, Transaction};
use ovaultd_crypto::verify_record;
use ovaultd_protocol::{
    Batch, BatchOp, ErrorCode, Op, OperationId, MAX_BATCH_OPS, MAX_FILE_BYTES,
};

use crate::error::{Result, ServerError};

#[derive(Debug, Clone, PartialEq)]
pub struct CommitOutcome {
    pub rev: i64,
    pub idempotente: bool,
}

#[derive(Debug, FromRow)]
struct VaultRow {
    epoch: i64,
    max_file_bytes: Option<i64>,
    vault_pubkey: Vec<u8>,
}

fn protocol_bad_request() -> ServerError {
    ServerError::Protocol(ErrorCode::BadRequest)
}

fn decode_b64(value: &str) -> Result<Vec<u8>> {
    STANDARD
        .decode(value)
        .map_err(|_| protocol_bad_request())
}

fn encode_id<T: Into<Vec<u8>>>(value: T) -> Vec<u8> {
    value.into()
}

/// Un batch puede traer varias operaciones sobre el mismo `path_id`: el caso normal es
/// `move A->X` + `mod X`, que el protocolo manda junto para que el rename y la edición
/// caigan en la misma revisión. `revisions` tiene PK `(vault_id, rev, path_id)`, así que
/// insertar las dos violaba la PK: el commit devolvía 500 y el reintento idéntico volvía a
/// fallar, dejando al device sin poder pushear nunca más. Se aplica sólo la última
/// operación de cada path (el estado final es el que importa) y se conserva el
/// `prev_path_id` de un move previo para no perder el rename.
fn colapsar_ops(ops: &[BatchOp]) -> Vec<BatchOp> {
    use std::collections::HashMap;

    let mut resultado: Vec<BatchOp> = Vec::with_capacity(ops.len());
    let mut posicion: HashMap<Vec<u8>, usize> = HashMap::new();
    for op in ops {
        let clave = op.path_id.as_bytes().to_vec();
        match posicion.get(&clave) {
            Some(&i) => {
                let previo = resultado[i].prev_path_id;
                resultado[i] = op.clone();
                if resultado[i].prev_path_id.is_none() {
                    resultado[i].prev_path_id = previo;
                }
            }
            None => {
                posicion.insert(clave, resultado.len());
                resultado.push(op.clone());
            }
        }
    }
    resultado
}

fn op_name(op: Op) -> &'static str {
    match op {
        Op::Add => "add",
        Op::Mod => "mod",
        Op::Del => "del",
        Op::Move => "move",
    }
}

fn checked_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| protocol_bad_request())
}

fn validate_operation_shape(op: &BatchOp) -> Result<()> {
    if op.sig.is_empty() {
        return Err(protocol_bad_request());
    }

    match op.op {
        Op::Del => {
            if op.blob_id.is_some()
                || op.path_enc.is_some()
                || op.t_mac.is_some()
                || op.size.is_some()
                || op.mtime.is_some()
                || op.ctime.is_some()
                || op.prev_path_id.is_some()
            {
                return Err(protocol_bad_request());
            }
        }

        Op::Add | Op::Mod => {
            if op.path_enc.is_none() || op.prev_path_id.is_some() {
                return Err(protocol_bad_request());
            }

            if op.folder {
                if op.blob_id.is_some() || op.size.is_some() || op.t_mac.is_some() {
                    return Err(protocol_bad_request());
                }
            } else if op.blob_id.is_none() || op.size.is_none() || op.t_mac.is_none() {
                return Err(protocol_bad_request());
            }
        }

        Op::Move => {
            if op.path_enc.is_none() || op.prev_path_id.is_none() {
                return Err(protocol_bad_request());
            }

            if op.folder {
                if op.blob_id.is_some() || op.size.is_some() || op.t_mac.is_some() {
                    return Err(protocol_bad_request());
                }
            } else if op.blob_id.is_none() || op.size.is_none() || op.t_mac.is_none() {
                return Err(protocol_bad_request());
            }
        }
    }

    Ok(())
}

fn verify_batch_operation(
    op: &BatchOp,
    vault_pubkey: &[u8; 32],
    epoch: u16,
    expected_rev: u64,
    operation_id: &[u8; 16],
) -> Result<()> {
    let blob_id = op
        .blob_id
        .map(|blob_id| *blob_id.as_bytes())
        .unwrap_or([0u8; 32]);

    let t_mac = match op.t_mac.as_deref() {
        Some(value) => {
            let decoded = STANDARD
                .decode(value)
                .map_err(|_| ServerError::Unauthorized)?;
            decoded
                .try_into()
                .map_err(|_| ServerError::Unauthorized)?
        }
        None => [0u8; 64],
    };

    let signature = STANDARD
        .decode(&op.sig)
        .map_err(|_| ServerError::Unauthorized)?;

    let op_code = match op.op {
        Op::Add => 0,
        Op::Mod => 1,
        Op::Del => 2,
        Op::Move => 3,
    };

    verify_record(
        vault_pubkey,
        op.path_id.as_bytes(),
        &blob_id,
        &t_mac,
        op_code,
        op.size.unwrap_or(0),
        op.mtime.unwrap_or(0),
        expected_rev,
        operation_id,
        epoch,
        &signature,
    )
    .map_err(|_| ServerError::Unauthorized)
}

async fn increment_blob_refs(
    executor: &mut Transaction<'_, Sqlite>,
    vault_id: i64,
    hash: &[u8],
) -> Result<()> {
    sqlx::query(
        "UPDATE blobs
         SET refs = refs + 1
         WHERE vault_id = ? AND hash = ?",
    )
    .bind(vault_id)
    .bind(hash)
    .execute(&mut **executor)
    .await?;

    Ok(())
}

async fn insert_revision(
    executor: &mut Transaction<'_, Sqlite>,
    vault_id: i64,
    rev: i64,
    op: &BatchOp,
    epoch: i64,
    device_id: i64,
    expected_rev: i64,
    operation_id: &str,
    path_enc: Option<&[u8]>,
    prev_path_id: Option<&[u8]>,
    blob_hash: Option<&[u8]>,
    t_mac: Option<&[u8]>,
    size: Option<i64>,
    mtime: Option<i64>,
    sig: &[u8],
) -> Result<()> {
    sqlx::query(
        "INSERT INTO revisions
             (vault_id, rev, path_id, op, path_enc, prev_path_id,
              blob_hash, t_mac, sig, epoch, size, mtime, device_id,
              operation_id, expected_rev)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(vault_id)
    .bind(rev)
    .bind(encode_id(op.path_id))
    .bind(op_name(op.op))
    .bind(path_enc)
    .bind(prev_path_id)
    .bind(blob_hash)
    .bind(t_mac)
    .bind(sig)
    .bind(epoch)
    .bind(size)
    .bind(mtime)
    .bind(device_id)
    .bind(operation_id)
    .bind(expected_rev)
    .execute(&mut **executor)
    .await?;

    Ok(())
}

async fn upsert_file(
    executor: &mut Transaction<'_, Sqlite>,
    vault_id: i64,
    path_id: &[u8],
    path_enc: Option<&[u8]>,
    folder: bool,
    blob_hash: Option<&[u8]>,
    t_mac: Option<&[u8]>,
    size: Option<i64>,
    mtime: Option<i64>,
    ctime: Option<i64>,
    rev: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO files
             (vault_id, path_id, path_enc, is_folder, blob_hash, t_mac,
              size, mtime, ctime, rev, deleted)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)
         ON CONFLICT(vault_id, path_id) DO UPDATE SET
             path_enc = excluded.path_enc,
             is_folder = excluded.is_folder,
             blob_hash = excluded.blob_hash,
             t_mac = excluded.t_mac,
             size = excluded.size,
             mtime = excluded.mtime,
             ctime = excluded.ctime,
             rev = excluded.rev,
             deleted = 0",
    )
    .bind(vault_id)
    .bind(path_id)
    .bind(path_enc)
    .bind(if folder { 1_i64 } else { 0_i64 })
    .bind(blob_hash)
    .bind(t_mac)
    .bind(size)
    .bind(mtime)
    .bind(ctime)
    .bind(rev)
    .execute(&mut **executor)
    .await?;

    Ok(())
}

async fn tombstone_file(
    executor: &mut Transaction<'_, Sqlite>,
    vault_id: i64,
    path_id: &[u8],
    rev: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO files
             (vault_id, path_id, path_enc, is_folder, blob_hash, t_mac,
              size, mtime, ctime, rev, deleted)
         VALUES (?, ?, NULL, 0, NULL, NULL, NULL, NULL, NULL, ?, 1)
         ON CONFLICT(vault_id, path_id) DO UPDATE SET
             path_enc = NULL,
             blob_hash = NULL,
             t_mac = NULL,
             size = NULL,
             mtime = NULL,
             ctime = NULL,
             rev = excluded.rev,
             deleted = 1",
    )
    .bind(vault_id)
    .bind(path_id)
    .bind(rev)
    .execute(&mut **executor)
    .await?;

    Ok(())
}

fn is_batch_operation_unique_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database_error) => {
            database_error
                .code()
                .as_deref()
                .is_some_and(|code| code == "2067" || code == "19")
                || database_error
                    .message()
                    .contains("idx_batches_operation")
        }
        _ => false,
    }
}

async fn existing_batch_revision(
    pool: &SqlitePool,
    vault_id: i64,
    operation_id: &str,
) -> Result<Option<i64>> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT rev
         FROM batches
         WHERE vault_id = ? AND operation_id = ?
         LIMIT 1",
    )
    .bind(vault_id)
    .bind(operation_id)
    .fetch_optional(pool)
    .await?)
}

/// ¿El batch repite EXACTAMENTE las operaciones que ya se guardaron para ese `operation_id`?
///
/// El id es la clave de idempotencia, pero un id reusado con otras ops no es un reintento:
/// es un cliente con estado viejo (backup restaurado) o un bug que repite el id. Sin esta
/// comparación, el server respondía `idempotente: true` y **descartaba los cambios nuevos**.
async fn mismas_ops(
    pool: &SqlitePool,
    vault_id: i64,
    operation_id: &str,
    ops: &[BatchOp],
) -> Result<bool> {
    let guardadas: Vec<(Vec<u8>, String, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT path_id, op, blob_hash
           FROM revisions
          WHERE vault_id = ? AND operation_id = ?",
    )
    .bind(vault_id)
    .bind(operation_id)
    .fetch_all(pool)
    .await?;

    // Se compara contra lo que REALMENTE se aplicó (un batch colapsa las ops por path).
    let mut esperadas: Vec<(Vec<u8>, String, Option<Vec<u8>>)> = colapsar_ops(ops)
        .into_iter()
        .map(|op| {
            (
                op.path_id.as_bytes().to_vec(),
                op_name(op.op).to_owned(),
                op.blob_id.map(|blob| blob.as_bytes().to_vec()),
            )
        })
        .collect();

    if guardadas.len() != esperadas.len() {
        return Ok(false);
    }
    let mut ordenadas: Vec<(Vec<u8>, String, Option<Vec<u8>>)> = guardadas;
    ordenadas.sort();
    esperadas.sort();
    Ok(ordenadas == esperadas)
}

pub async fn commit_batch(
    pool: &SqlitePool,
    vault_id: i64,
    device_id: i64,
    batch: &Batch,
) -> Result<CommitOutcome> {
    if batch.ops.is_empty() || batch.ops.len() > MAX_BATCH_OPS {
        return Err(protocol_bad_request());
    }

    for op in &batch.ops {
        validate_operation_shape(op)?;
    }

    let vault = sqlx::query_as::<_, VaultRow>(
        "SELECT epoch, max_file_bytes, vault_pubkey
         FROM vaults
         WHERE id = ?",
    )
    .bind(vault_id)
    .fetch_optional(pool)
    .await?
    .ok_or(ServerError::NotFound)?;

    let vault_pubkey: [u8; 32] = vault
        .vault_pubkey
        .as_slice()
        .try_into()
        .map_err(|_| ServerError::Unauthorized)?;

    let epoch = u16::try_from(vault.epoch).map_err(|_| ServerError::Unauthorized)?;
    let operation_id = OperationId::from_wire(&batch.operation_id)
        .map_err(|_| ServerError::Unauthorized)?;

    for op in &batch.ops {
        verify_batch_operation(
            op,
            &vault_pubkey,
            epoch,
            batch.expected_rev,
            operation_id.as_bytes(),
        )?;
    }

    let existing_rev = existing_batch_revision(pool, vault_id, &batch.operation_id).await?;

    if let Some(rev) = existing_rev {
        // Un reintento legítimo trae las mismas ops. Uno con el mismo id y otras ops es un
        // cliente con estado viejo: devolverle la rev anterior le diría "aplicado" mientras
        // sus cambios se pierden.
        if !mismas_ops(pool, vault_id, &batch.operation_id, &batch.ops).await? {
            return Err(ServerError::Invalid(
                "el operation_id ya se usó con otras operaciones".to_owned(),
            ));
        }
        return Ok(CommitOutcome {
            rev,
            idempotente: true,
        });
    }

    // Dedupe por path: ver `colapsar_ops`. La verificación de firmas de arriba corre
    // sobre TODAS las ops del mensaje; acá sólo se aplica la última de cada path.
    let ops = colapsar_ops(&batch.ops);

    let max_file_bytes = vault
        .max_file_bytes
        .map(|value| u64::try_from(value).unwrap_or(0))
        .unwrap_or(MAX_FILE_BYTES)
        .min(MAX_FILE_BYTES);

    for op in &ops {
        if let Some(size) = op.size {
            if size > max_file_bytes {
                return Err(ServerError::Protocol(ErrorCode::MaxFileExceeded));
            }
        }
    }

    let mut tx = pool.begin().await?;

    // Tomar el write lock ANTES de leer nada. sqlx abre las transacciones de SQLite en modo
    // deferred: dos commits concurrentes leían el mismo head, los dos pasaban el chequeo de
    // `expected_rev` y el perdedor terminaba en 500 (PK de `batches` o BUSY_SNAPSHOT, que el
    // busy_timeout no reintenta). Un UPDATE no-op serializa los commits acá; el segundo
    // espera (busy_timeout) y cuando entra ve el head que dejó el primero.
    sqlx::query("UPDATE vaults SET head_rev = head_rev WHERE id = ?")
        .bind(vault_id)
        .execute(&mut *tx)
        .await?;

    for op in &ops {
        if let Some(blob_id) = op.blob_id {
            if !crate::storage::blob_registrado(&mut *tx, vault_id, &blob_id).await? {
                return Err(ServerError::NotFound);
            }
        }
    }

    let expected_rev = checked_i64(batch.expected_rev)?;

    // El head se relee DENTRO de la transacción (ya con el lock): la foto de `vault` se
    // tomó antes de esperar el lock y puede estar vieja.
    let current_head: i64 = sqlx::query_scalar("SELECT head_rev FROM vaults WHERE id = ?")
        .bind(vault_id)
        .fetch_one(&mut *tx)
        .await?;

    if expected_rev != current_head {
        return Err(ServerError::Conflict {
            path_id: ops[0].path_id,
            expected: u64::try_from(expected_rev).unwrap_or(0),
            head: u64::try_from(current_head).unwrap_or(0),
        });
    }

    let rev = current_head
        .checked_add(1)
        .ok_or_else(|| ServerError::Invalid("revisión fuera de rango".to_owned()))?;

    for op in &ops {
        let path_id = op.path_id.as_bytes().as_slice();
        let path_enc = op
            .path_enc
            .as_deref()
            .map(decode_b64)
            .transpose()?;
        let t_mac = op.t_mac.as_deref().map(decode_b64).transpose()?;
        let sig = decode_b64(&op.sig)?;

        let blob_hash = op.blob_id.map(|blob_id| blob_id.as_bytes().to_vec());
        let size = op.size.map(checked_i64).transpose()?;
        let mtime = op.mtime.map(checked_i64).transpose()?;
        let ctime = op.ctime.map(checked_i64).transpose()?;

        match op.op {
            Op::Add | Op::Mod => {
                // Un rename+edit llega como move + mod del mismo path_id y el colapso deja
                // el mod heredando el `prev_path_id`: hay que tombstonear el origen igual
                // que en un move, o el rename queda a medias (el origen sigue vivo).
                let previous_path: Option<Vec<u8>> = match op.prev_path_id {
                    Some(previous_path_id) => {
                        let previous = previous_path_id.as_bytes().to_vec();
                        if previous.as_slice() == path_id {
                            return Err(protocol_bad_request());
                        }
                        let existe = sqlx::query_scalar::<_, i64>(
                            "SELECT 1
                             FROM files
                             WHERE vault_id = ? AND path_id = ? AND deleted = 0
                             LIMIT 1",
                        )
                        .bind(vault_id)
                        .bind(previous.as_slice())
                        .fetch_optional(&mut *tx)
                        .await?;
                        if existe.is_none() {
                            return Err(ServerError::NotFound);
                        }
                        Some(previous)
                    }
                    None => None,
                };

                upsert_file(
                    &mut tx,
                    vault_id,
                    path_id,
                    path_enc.as_deref(),
                    op.folder,
                    blob_hash.as_deref(),
                    t_mac.as_deref(),
                    size,
                    mtime,
                    ctime,
                    rev,
                )
                .await?;

                if let Some(previous) = previous_path.as_deref() {
                    tombstone_file(&mut tx, vault_id, previous, rev).await?;
                }

                insert_revision(
                    &mut tx,
                    vault_id,
                    rev,
                    op,
                    vault.epoch,
                    device_id,
                    expected_rev,
                    batch.operation_id.as_str(),
                    path_enc.as_deref(),
                    previous_path.as_deref(),
                    blob_hash.as_deref(),
                    t_mac.as_deref(),
                    size,
                    mtime,
                    &sig,
                )
                .await?;

                if let Some(hash) = blob_hash.as_deref() {
                    increment_blob_refs(&mut tx, vault_id, hash).await?;
                }
            }

            Op::Del => {
                tombstone_file(&mut tx, vault_id, path_id, rev).await?;

                insert_revision(
                    &mut tx,
                    vault_id,
                    rev,
                    op,
                    vault.epoch,
                    device_id,
                    expected_rev,
                    batch.operation_id.as_str(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    &sig,
                )
                .await?;
            }

            Op::Move => {
                let previous_path_id = op
                    .prev_path_id
                    .ok_or_else(protocol_bad_request)?;
                let previous_path = previous_path_id.as_bytes().as_slice();

                // Renombrar un path sobre sí mismo no es un move: con el upsert y el
                // tombstone del mismo path_id en la misma revisión el archivo desaparece.
                if previous_path == path_id {
                    return Err(protocol_bad_request());
                }

                // `deleted = 0`: mover desde un tombstone resucitaba un blob que el GC ya
                // podía haber recolectado (el índice afirmaba que existía).
                let previous_exists = sqlx::query_scalar::<_, i64>(
                    "SELECT 1
                     FROM files
                     WHERE vault_id = ? AND path_id = ? AND deleted = 0
                     LIMIT 1",
                )
                .bind(vault_id)
                .bind(previous_path)
                .fetch_optional(&mut *tx)
                .await?;

                if previous_exists.is_none() {
                    return Err(ServerError::NotFound);
                }

                upsert_file(
                    &mut tx,
                    vault_id,
                    path_id,
                    path_enc.as_deref(),
                    op.folder,
                    blob_hash.as_deref(),
                    t_mac.as_deref(),
                    size,
                    mtime,
                    ctime,
                    rev,
                )
                .await?;

                tombstone_file(&mut tx, vault_id, previous_path, rev).await?;

                insert_revision(
                    &mut tx,
                    vault_id,
                    rev,
                    op,
                    vault.epoch,
                    device_id,
                    expected_rev,
                    batch.operation_id.as_str(),
                    path_enc.as_deref(),
                    Some(previous_path),
                    blob_hash.as_deref(),
                    t_mac.as_deref(),
                    size,
                    mtime,
                    &sig,
                )
                .await?;

                if let Some(hash) = blob_hash.as_deref() {
                    increment_blob_refs(&mut tx, vault_id, hash).await?;
                }
            }
        }
    }

    let batch_insert = sqlx::query(
        "INSERT INTO batches
             (vault_id, rev, batch_id, operation_id, op_count, device_id)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(vault_id)
    .bind(rev)
    .bind(&batch.batch_id)
    .bind(&batch.operation_id)
    .bind(i64::try_from(ops.len()).map_err(|_| protocol_bad_request())?)
    .bind(device_id)
    .execute(&mut *tx)
    .await;

    if let Err(error) = batch_insert {
        if is_batch_operation_unique_violation(&error) {
            // Esta rama se verifica por lectura, no por test: una carrera real
            // entre dos commits no se puede forzar de forma determinista en un
            // test en proceso.
            tx.rollback().await?;

            if let Some(existing_rev) =
                existing_batch_revision(pool, vault_id, &batch.operation_id).await?
            {
                return Ok(CommitOutcome {
                    rev: existing_rev,
                    idempotente: true,
                });
            }
        }

        return Err(error.into());
    }

    let updated = sqlx::query(
        "UPDATE vaults
         SET head_rev = ?
         WHERE id = ? AND head_rev = ?",
    )
    .bind(rev)
    .bind(vault_id)
    .bind(current_head)
    .execute(&mut *tx)
    .await?;

    if updated.rows_affected() != 1 {
        tx.rollback().await?;

        let head = sqlx::query_scalar::<_, i64>(
            "SELECT head_rev
             FROM vaults
             WHERE id = ?",
        )
        .bind(vault_id)
        .fetch_optional(pool)
        .await?
        .ok_or(ServerError::NotFound)?;

        return Err(ServerError::Conflict {
            path_id: ops[0].path_id,
            expected: u64::try_from(expected_rev).unwrap_or(0),
            head: u64::try_from(head).unwrap_or(0),
        });
    }

    tx.commit().await?;

    // Failpoint de test: crash DESPUÉS del commit SQL y antes de responder el ack. La
    // revisión quedó confirmada; el reintento del mismo `operation_id` debe ser idempotente.
    crate::failpoints::failpoint("sql.commit");

    Ok(CommitOutcome {
        rev,
        idempotente: false,
    })
}

#[cfg(test)]
mod tests;

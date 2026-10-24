use super::*;

use base64::engine::general_purpose::STANDARD;
use std::path::PathBuf;
use ovaultd_crypto::{derive_subkeys, sign_record, VaultIdentity, VaultKey};
use ovaultd_protocol::{BlobId, OperationId, PathId};
use uuid::Uuid;

const TEST_VAULT_KEY: VaultKey = VaultKey([7u8; 32]);
const TEST_VAULT_SALT: [u8; 16] = [0u8; 16];

struct TestContext {
    pool: SqlitePool,
    path: PathBuf,
    vault_id: i64,
    device_id: i64,
    epoch: u16,
}

impl TestContext {
    async fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "ovaultd-server-commit-{}.db",
            Uuid::new_v4()
        ));
        let path_string = path
            .to_str()
            .ok_or("la ruta temporal no es UTF-8")?
            .to_owned();

        let pool = crate::db::connect(&path_string).await?;
        sqlx::migrate!("../../migrations").run(&pool).await?;

        let account_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO accounts (email, password_hash) VALUES (?, ?) RETURNING id",
        )
        .bind(format!("{}@example.com", Uuid::new_v4()))
        .bind(crate::auth::phc_de_prueba())
        .fetch_one(&pool)
        .await?;

        let keys = derive_subkeys(&TEST_VAULT_KEY, &TEST_VAULT_SALT);
        let vault_pubkey = VaultIdentity::generate(&keys.sign).pubkey();

        let vault_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO vaults
                 (account_id, name, salt, kdf_params, wrapped_key_pw,
                  wrapped_key_rc, vault_pubkey, max_file_bytes)
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL)
             RETURNING id",
        )
        .bind(account_id)
        .bind("test-vault")
        .bind(TEST_VAULT_SALT.to_vec())
        .bind("{}")
        .bind(vec![0u8; 32])
        .bind(vec![0u8; 32])
        .bind(vault_pubkey.to_vec())
        .fetch_one(&pool)
        .await?;

        let epoch = sqlx::query_scalar::<_, i64>(
            "SELECT epoch
             FROM vaults
             WHERE id = ?",
        )
        .bind(vault_id)
        .fetch_one(&pool)
        .await?;
        let epoch = u16::try_from(epoch)?;

        let device_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO devices (account_id, name)
             VALUES (?, ?)
             RETURNING id",
        )
        .bind(account_id)
        .bind("test-device")
        .fetch_one(&pool)
        .await?;

        Ok(Self {
            pool,
            path,
            vault_id,
            device_id,
            epoch,
        })
    }

    async fn insert_blob(
        &self,
        blob_id: BlobId,
        size: i64,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        sqlx::query(
            "INSERT INTO blobs (vault_id, hash, size)
             VALUES (?, ?, ?)",
        )
        .bind(self.vault_id)
        .bind(blob_id.as_bytes().as_slice())
        .bind(size)
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

fn path(value: u8) -> PathId {
    PathId::new([value; 32])
}

fn blob(value: u8) -> BlobId {
    BlobId::new([value; 32])
}

fn zero_t_mac() -> String {
    STANDARD.encode([0u8; 64])
}

fn canonical_operation_id(value: &str) -> String {
    let mut bytes = [0u8; 16];

    for (index, byte) in value.as_bytes().iter().copied().take(16).enumerate() {
        bytes[index] = byte;
    }

    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn op_code(op: Op) -> u8 {
    match op {
        Op::Add => 0,
        Op::Mod => 1,
        Op::Del => 2,
        Op::Move => 3,
    }
}

fn sign_op(
    op: &mut BatchOp,
    identity: &VaultIdentity,
    expected_rev: u64,
    operation_id: &[u8; 16],
    epoch: u16,
) {
    let blob_id = op
        .blob_id
        .map(|value| *value.as_bytes())
        .unwrap_or([0u8; 32]);

    let t_mac = [0u8; 64];

    let signature = sign_record(
        identity,
        op.path_id.as_bytes(),
        &blob_id,
        &t_mac,
        op_code(op.op),
        op.size.unwrap_or(0),
        op.mtime.unwrap_or(0),
        expected_rev,
        operation_id,
        epoch,
    );

    op.sig = STANDARD.encode(signature);
}

fn sign_batch_with_identity(
    mut request: Batch,
    identity: &VaultIdentity,
    epoch: u16,
) -> Batch {
    let expected_rev = request.expected_rev;
    let operation_id = OperationId::from_wire(&request.operation_id)
        .expect("los helpers de test deben producir operation_id válidos");

    for op in &mut request.ops {
        sign_op(op, identity, expected_rev, operation_id.as_bytes(), epoch);
    }

    request
}

fn add_op(path_id: PathId, blob_id: BlobId) -> BatchOp {
    BatchOp {
        path_id,
        op: Op::Add,
        path_enc: Some("bm9tYnJl".to_owned()),
        folder: false,
        blob_id: Some(blob_id),
        t_mac: Some(zero_t_mac()),
        size: Some(10),
        mtime: Some(1),
        ctime: Some(1),
        prev_path_id: None,
        sig: String::new(),
    }
}

fn mod_op(path_id: PathId, blob_id: BlobId) -> BatchOp {
    BatchOp {
        path_id,
        op: Op::Mod,
        path_enc: Some("bm9tYnJl".to_owned()),
        folder: false,
        blob_id: Some(blob_id),
        t_mac: Some(zero_t_mac()),
        size: Some(10),
        mtime: Some(1),
        ctime: Some(1),
        prev_path_id: None,
        sig: String::new(),
    }
}

fn del_op(path_id: PathId) -> BatchOp {
    BatchOp {
        path_id,
        op: Op::Del,
        path_enc: None,
        folder: false,
        blob_id: None,
        t_mac: None,
        size: None,
        mtime: None,
        ctime: None,
        prev_path_id: None,
        sig: String::new(),
    }
}

fn move_op(path_id: PathId, previous_path_id: PathId, blob_id: BlobId) -> BatchOp {
    BatchOp {
        path_id,
        op: Op::Move,
        path_enc: Some("bm9tYnJl".to_owned()),
        folder: false,
        blob_id: Some(blob_id),
        t_mac: Some(zero_t_mac()),
        size: Some(10),
        mtime: Some(1),
        ctime: Some(1),
        prev_path_id: Some(previous_path_id),
        sig: String::new(),
    }
}

fn batch(
    operation_id: &str,
    expected_rev: u64,
    epoch: u16,
    ops: Vec<BatchOp>,
) -> Batch {
    let request = Batch {
        t: "batch".to_string(),
        operation_id: canonical_operation_id(operation_id),
        batch_id: format!("batch-{operation_id}"),
        expected_rev,
        ops,
    };

    let keys = derive_subkeys(&TEST_VAULT_KEY, &TEST_VAULT_SALT);
    let identity = VaultIdentity::generate(&keys.sign);

    sign_batch_with_identity(request, &identity, epoch)
}

#[tokio::test]
async fn un_batch_de_n_ops_es_una_sola_revision(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let blob_id = blob(0x03);
    context.insert_blob(blob_id, 10).await?;

    let request = batch(
        "op-batch-1",
        0,
        context.epoch,
        vec![
            mod_op(path(0x10), blob_id),
            move_op(path(0x11), path(0x12), blob_id),
            del_op(path(0x13)),
        ],
    );

    sqlx::query(
        "INSERT INTO files
             (vault_id, path_id, path_enc, blob_hash, t_mac, size,
              mtime, ctime, rev, deleted)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
    )
    .bind(context.vault_id)
    .bind(path(0x12).as_bytes().as_slice())
    .bind(vec![1u8])
    .bind(blob_id.as_bytes().as_slice())
    .bind(vec![1u8; 64])
    .bind(10_i64)
    .bind(1_i64)
    .bind(1_i64)
    .bind(0_i64)
    .execute(&context.pool)
    .await?;

    let result = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &request,
    )
    .await?;

    let revisions = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)
         FROM revisions
         WHERE vault_id = ? AND rev = ?",
    )
    .bind(context.vault_id)
    .bind(result.rev)
    .fetch_one(&context.pool)
    .await?;

    let distinct_revs = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(DISTINCT rev)
         FROM revisions
         WHERE vault_id = ?",
    )
    .bind(context.vault_id)
    .fetch_one(&context.pool)
    .await?;

    let head_rev = sqlx::query_scalar::<_, i64>(
        "SELECT head_rev FROM vaults WHERE id = ?",
    )
    .bind(context.vault_id)
    .fetch_one(&context.pool)
    .await?;

    let op_count = sqlx::query_scalar::<_, i64>(
        "SELECT op_count
         FROM batches
         WHERE vault_id = ? AND rev = ?",
    )
    .bind(context.vault_id)
    .bind(result.rev)
    .fetch_one(&context.pool)
    .await?;

    assert_eq!(
        revisions,
        3,
        "la revisión {} debía contener 3 filas y contiene {}",
        result.rev,
        revisions
    );
    assert_eq!(
        distinct_revs,
        1,
        "las 3 operaciones debían compartir una sola revisión y hay {}",
        distinct_revs
    );
    assert_eq!(
        head_rev,
        1,
        "head_rev debía avanzar a 1 y quedó en {}",
        head_rev
    );
    assert_eq!(
        op_count,
        3,
        "batches.op_count debía ser 3 y es {}",
        op_count
    );

    Ok(())
}

#[tokio::test]
async fn batch_con_operation_id_repetido_devuelve_la_misma_rev(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let blob_id = blob(0x03);
    context.insert_blob(blob_id, 10).await?;

    let request = batch(
        "op-repetida",
        0,
        context.epoch,
        vec![add_op(path(0x10), blob_id)],
    );

    let first = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &request,
    )
    .await?;

    let before = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM revisions WHERE vault_id = ?",
    )
    .bind(context.vault_id)
    .fetch_one(&context.pool)
    .await?;

    let second = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &request,
    )
    .await?;

    let after = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM revisions WHERE vault_id = ?",
    )
    .bind(context.vault_id)
    .fetch_one(&context.pool)
    .await?;

    assert!(
        second.idempotente,
        "el segundo batch debía ser idempotente y devolvió {:?}",
        second.idempotente
    );
    assert_eq!(
        second.rev,
        first.rev,
        "el retry debía devolver rev {} y devolvió {}",
        first.rev,
        second.rev
    );
    assert_eq!(
        after,
        before,
        "el retry no debía insertar revisiones: antes {} y después {}",
        before,
        after
    );

    Ok(())
}

#[tokio::test]
async fn concurrent_expected_rev_has_one_winner(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let blob_id = blob(0x03);
    context.insert_blob(blob_id, 10).await?;

    let first = batch(
        "concurrent-1",
        0,
        context.epoch,
        vec![add_op(path(0x10), blob_id)],
    );
    let second_path = path(0x11);
    let second = batch(
        "concurrent-2",
        0,
        context.epoch,
        vec![add_op(second_path, blob_id)],
    );

    let winner = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &first,
    )
    .await?;

    let loser = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &second,
    )
    .await;

    assert_eq!(
        winner.rev,
        1,
        "el primer batch debía ganar con rev 1 y obtuvo {}",
        winner.rev
    );
    assert!(
        matches!(
            loser,
            Err(ServerError::Conflict {
                path_id,
                expected: 0,
                head: 1
            }) if path_id == second_path
        ),
        "el segundo batch debía dar Conflict con path_id {:?} y devolvió {:?}",
        second_path,
        loser
    );

    Ok(())
}

#[tokio::test]
async fn retry_after_lost_ack_returns_same_revision(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let blob_id = blob(0x03);
    context.insert_blob(blob_id, 10).await?;

    let first = batch(
        "lost-ack",
        0,
        context.epoch,
        vec![add_op(path(0x10), blob_id)],
    );
    let first_result = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &first,
    )
    .await?;

    let second = batch(
        "after-lost-ack",
        1,
        context.epoch,
        vec![add_op(path(0x11), blob_id)],
    );
    commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &second,
    )
    .await?;

    let retry = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &first,
    )
    .await?;

    assert!(
        retry.idempotente,
        "el retry debía ser idempotente y devolvió {:?}",
        retry.idempotente
    );
    assert_eq!(
        retry.rev,
        first_result.rev,
        "el retry debía devolver rev {} y devolvió {}",
        first_result.rev,
        retry.rev
    );

    Ok(())
}

#[tokio::test]
async fn move_marca_el_path_viejo_y_no_inventa_filas(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let blob_id = blob(0x03);
    let old_path = path(0x10);
    let new_path = path(0x11);
    context.insert_blob(blob_id, 10).await?;

    sqlx::query(
        "INSERT INTO files
             (vault_id, path_id, path_enc, blob_hash, t_mac, size,
              mtime, ctime, rev, deleted)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
    )
    .bind(context.vault_id)
    .bind(old_path.as_bytes().as_slice())
    .bind(vec![1u8])
    .bind(blob_id.as_bytes().as_slice())
    .bind(vec![1u8; 64])
    .bind(10_i64)
    .bind(1_i64)
    .bind(1_i64)
    .bind(0_i64)
    .execute(&context.pool)
    .await?;

    let request = batch(
        "move-only",
        0,
        context.epoch,
        vec![move_op(new_path, old_path, blob_id)],
    );

    let result = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &request,
    )
    .await?;

    let deleted = sqlx::query_scalar::<_, i64>(
        "SELECT deleted
         FROM files
         WHERE vault_id = ? AND path_id = ?",
    )
    .bind(context.vault_id)
    .bind(old_path.as_bytes().as_slice())
    .fetch_one(&context.pool)
    .await?;

    let derived_deletes = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)
         FROM revisions
         WHERE vault_id = ? AND rev = ? AND path_id = ? AND op = 'del'",
    )
    .bind(context.vault_id)
    .bind(result.rev)
    .bind(old_path.as_bytes().as_slice())
    .fetch_one(&context.pool)
    .await?;

    assert_eq!(
        deleted,
        1,
        "el path viejo debía quedar deleted=1 y quedó {}",
        deleted
    );
    assert_eq!(
        derived_deletes,
        0,
        "move no debía inventar una revisión del para el path viejo y creó {}",
        derived_deletes
    );

    Ok(())
}

#[tokio::test]
async fn move_de_un_path_inexistente_da_not_found(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let blob_id = blob(0x03);
    context.insert_blob(blob_id, 10).await?;

    let request = batch(
        "move-missing",
        0,
        context.epoch,
        vec![move_op(path(0x11), path(0x12), blob_id)],
    );

    let result = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &request,
    )
    .await;

    assert!(
        matches!(result, Err(ServerError::NotFound)),
        "move de un path inexistente debía dar NotFound y devolvió {:?}",
        result
    );

    Ok(())
}

#[tokio::test]
async fn batch_sobre_max_batch_ops_da_bad_request(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let blob_id = blob(0x03);
    context.insert_blob(blob_id, 10).await?;

    let ops = (0..=MAX_BATCH_OPS)
        .map(|index| del_op(path((index % 255) as u8)))
        .collect();

    let request = batch("too-many-ops", 0, context.epoch, ops);
    let result = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &request,
    )
    .await;

    let revisions = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM revisions WHERE vault_id = ?",
    )
    .bind(context.vault_id)
    .fetch_one(&context.pool)
    .await?;

    assert!(
        matches!(result, Err(ServerError::Protocol(ErrorCode::BadRequest))),
        "batch sobre MAX_BATCH_OPS debía dar BadRequest y devolvió {:?}",
        result
    );
    assert_eq!(
        revisions,
        0,
        "el batch rechazado no debía crear revisiones y creó {}",
        revisions
    );

    Ok(())
}

#[tokio::test]
async fn blob_inexistente_da_not_found(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let request = batch(
        "missing-blob",
        0,
        context.epoch,
        vec![add_op(path(0x10), blob(0x03))],
    );

    let result = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &request,
    )
    .await;

    assert!(
        matches!(result, Err(ServerError::NotFound)),
        "blob inexistente debía dar NotFound y devolvió {:?}",
        result
    );

    Ok(())
}

#[tokio::test]
async fn la_revision_guardada_conserva_el_expected_rev_firmado(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let first_blob = blob(0x01);
    let second_blob = blob(0x02);
    let third_blob = blob(0x03);

    context.insert_blob(first_blob, 10).await?;
    context.insert_blob(second_blob, 10).await?;
    context.insert_blob(third_blob, 10).await?;

    let first = batch(
        "expected-rev-first",
        0,
        context.epoch,
        vec![add_op(path(0x10), first_blob)],
    );
    let first_result = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &first,
    )
    .await?;

    assert_eq!(
        first_result.rev,
        1,
        "el primer batch debía dejar el head en rev 1 y devolvió {}",
        first_result.rev
    );

    let second = batch(
        "expected-rev-second",
        1,
        context.epoch,
        vec![
            mod_op(path(0x10), second_blob),
            add_op(path(0x11), third_blob),
            del_op(path(0x12)),
        ],
    );
    let second_result = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &second,
    )
    .await?;

    let expected_revs = sqlx::query_scalar::<_, i64>(
        "SELECT expected_rev
         FROM revisions
         WHERE vault_id = ? AND rev = ?",
    )
    .bind(context.vault_id)
    .bind(second_result.rev)
    .fetch_all(&context.pool)
    .await?;

    assert_eq!(
        expected_revs.len(),
        3,
        "la revisión {} debía tener 3 filas y tiene {}",
        second_result.rev,
        expected_revs.len()
    );

    for found in expected_revs {
        assert_eq!(
            found, 1,
            "la revisión {} guardó expected_rev={} pero el contrato exige 1",
            second_result.rev, found
        );
        assert_ne!(
            found, 0,
            "la revisión {} no debía guardar expected_rev=0",
            second_result.rev
        );
    }

    Ok(())
}

#[tokio::test]
async fn los_refs_cuentan_cada_fila_de_revision_que_referencia_el_blob(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let shared_blob = blob(0x04);
    context.insert_blob(shared_blob, 10).await?;

    let request = batch(
        "shared-blob-two-revisions",
        0,
        context.epoch,
        vec![
            add_op(path(0x20), shared_blob),
            add_op(path(0x21), shared_blob),
            del_op(path(0x22)),
        ],
    );

    let result = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &request,
    )
    .await?;

    let refs = sqlx::query_scalar::<_, i64>(
        "SELECT refs
         FROM blobs
         WHERE vault_id = ? AND hash = ?",
    )
    .bind(context.vault_id)
    .bind(shared_blob.as_bytes().as_slice())
    .fetch_one(&context.pool)
    .await?;

    assert_eq!(
        refs, 2,
        "el blob usado por 2 filas de revisions debía tener refs=2 y tiene {} en rev {}",
        refs, result.rev
    );

    Ok(())
}

#[tokio::test]
async fn una_firma_invalida_no_se_persiste(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let blob_id = blob(0x05);
    context.insert_blob(blob_id, 10).await?;

    let mut request = batch(
        "invalid-signature",
        0,
        context.epoch,
        vec![add_op(path(0x30), blob_id)],
    );

    let mut signature = STANDARD.decode(&request.ops[0].sig)?;
    signature[0] ^= 1;
    request.ops[0].sig = STANDARD.encode(signature);

    let result = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &request,
    )
    .await;

    assert!(
        matches!(result, Err(ServerError::Unauthorized)),
        "una firma inválida debía dar Unauthorized y devolvió {:?}",
        result
    );

    let revisions = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM revisions WHERE vault_id = ?",
    )
    .bind(context.vault_id)
    .fetch_one(&context.pool)
    .await?;

    let batches = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM batches WHERE vault_id = ?",
    )
    .bind(context.vault_id)
    .fetch_one(&context.pool)
    .await?;

    assert_eq!(revisions, 0);
    assert_eq!(batches, 0);

    Ok(())
}

#[tokio::test]
async fn una_firma_de_otro_vault_no_vale(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let blob_id = blob(0x06);
    context.insert_blob(blob_id, 10).await?;

    let other_keys = derive_subkeys(&VaultKey([8u8; 32]), &TEST_VAULT_SALT);
    let other_identity = VaultIdentity::generate(&other_keys.sign);

    let request = sign_batch_with_identity(
        Batch {
            t: "batch".to_owned(),
            operation_id: canonical_operation_id("other-vault-signature"),
            batch_id: "batch-other-vault-signature".to_owned(),
            expected_rev: 0,
            ops: vec![add_op(path(0x31), blob_id)],
        },
        &other_identity,
        context.epoch,
    );

    let result = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &request,
    )
    .await;

    assert!(
        matches!(result, Err(ServerError::Unauthorized)),
        "una firma de otro vault debía dar Unauthorized y devolvió {:?}",
        result
    );

    let revisions = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM revisions WHERE vault_id = ?",
    )
    .bind(context.vault_id)
    .fetch_one(&context.pool)
    .await?;

    let batches = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM batches WHERE vault_id = ?",
    )
    .bind(context.vault_id)
    .fetch_one(&context.pool)
    .await?;

    assert_eq!(revisions, 0);
    assert_eq!(batches, 0);

    Ok(())
}


#[tokio::test]
async fn dos_ops_del_mismo_path_colapsan_en_una_revision(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let primero = blob(0x03);
    let segundo = blob(0x04);
    context.insert_blob(primero, 10).await?;
    context.insert_blob(segundo, 10).await?;

    // La outbox puede mandar dos ops del mismo path (un `pending` que se vuelve a editar).
    // Antes esto violaba la PK `(vault_id, rev, path_id)` y el device quedaba con 500 fijo.
    let request = batch(
        "op-duplicada",
        0,
        context.epoch,
        vec![
            add_op(path(0x10), primero),
            add_op(path(0x10), segundo),
        ],
    );

    let outcome = commit_batch(
        &context.pool,
        context.vault_id,
        context.device_id,
        &request,
    )
    .await?;

    assert_eq!(outcome.rev, 1, "el batch tenía que commitear una sola revisión");

    let filas: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM revisions WHERE vault_id = ? AND rev = 1",
    )
    .bind(context.vault_id)
    .fetch_one(&context.pool)
    .await?;
    assert_eq!(filas, 1, "una fila de revisión por path, no una por op");

    let (blob_hash, deleted): (Vec<u8>, i64) = sqlx::query_as(
        "SELECT blob_hash, deleted FROM files WHERE vault_id = ? AND path_id = ?",
    )
    .bind(context.vault_id)
    .bind(path(0x10).as_bytes().as_slice())
    .fetch_one(&context.pool)
    .await?;
    assert_eq!(
        blob_hash,
        segundo.as_bytes().to_vec(),
        "tiene que ganar la última op del path"
    );
    assert_eq!(deleted, 0);

    Ok(())
}

#[tokio::test]
async fn move_mas_edit_en_una_revision_conserva_el_origen_borrado(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let original = blob(0x03);
    let editado = blob(0x04);
    context.insert_blob(original, 10).await?;
    context.insert_blob(editado, 10).await?;

    let alta = batch(
        "op-alta-origen",
        0,
        context.epoch,
        vec![add_op(path(0x12), original)],
    );
    commit_batch(&context.pool, context.vault_id, context.device_id, &alta).await?;

    // `rename + edit` es UNA revisión según el protocolo: move A->X seguido de mod X.
    let request = batch(
        "op-rename-edit",
        1,
        context.epoch,
        vec![
            move_op(path(0x11), path(0x12), original),
            mod_op(path(0x11), editado),
        ],
    );

    commit_batch(&context.pool, context.vault_id, context.device_id, &request).await?;

    let filas: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM revisions WHERE vault_id = ? AND rev = 2",
    )
    .bind(context.vault_id)
    .fetch_one(&context.pool)
    .await?;
    assert_eq!(filas, 1, "rename + edit cae en una sola fila de revisión");

    let (prev, destino_blob): (Option<Vec<u8>>, Vec<u8>) = sqlx::query_as(
        "SELECT r.prev_path_id, f.blob_hash
         FROM revisions r
         JOIN files f ON f.vault_id = r.vault_id AND f.path_id = r.path_id
         WHERE r.vault_id = ? AND r.rev = 2 AND r.path_id = ?",
    )
    .bind(context.vault_id)
    .bind(path(0x11).as_bytes().as_slice())
    .fetch_one(&context.pool)
    .await?;
    assert_eq!(
        prev.as_deref(),
        Some(path(0x12).as_bytes().as_slice()),
        "el move colapsado tiene que seguir diciendo de dónde vino"
    );
    assert_eq!(destino_blob, editado.as_bytes().to_vec());

    let origen_deleted: i64 = sqlx::query_scalar(
        "SELECT deleted FROM files WHERE vault_id = ? AND path_id = ?",
    )
    .bind(context.vault_id)
    .bind(path(0x12).as_bytes().as_slice())
    .fetch_one(&context.pool)
    .await?;
    assert_eq!(origen_deleted, 1, "el origen del rename tiene que quedar tombstoneado");

    Ok(())
}

// No hay en este archivo ningún INSERT manual en `revisions`; las filas de
// `revisions` se crean únicamente mediante `commit.rs`, que ya incluye
// `operation_id` con el mismo formato que `batches.operation_id`.

#[tokio::test]
async fn dos_commits_concurrentes_uno_gana_y_el_otro_es_conflict(
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let primero = blob(0x03);
    let segundo = blob(0x04);
    context.insert_blob(primero, 10).await?;
    context.insert_blob(segundo, 10).await?;

    // Los dos construyen sobre el mismo head: es la carrera real de dos devices.
    let a = batch(
        "op-carrera-a",
        0,
        context.epoch,
        vec![add_op(path(0x10), primero)],
    );
    let b = batch(
        "op-carrera-b",
        0,
        context.epoch,
        vec![add_op(path(0x11), segundo)],
    );

    let (ra, rb) = tokio::join!(
        commit_batch(&context.pool, context.vault_id, context.device_id, &a),
        commit_batch(&context.pool, context.vault_id, context.device_id, &b),
    );

    let ganadores = usize::from(ra.is_ok()) + usize::from(rb.is_ok());
    assert_eq!(
        ganadores, 1,
        "exactamente uno tiene que ganar la carrera: {ra:?} / {rb:?}"
    );

    let perdedor = if ra.is_err() { &ra } else { &rb };
    assert!(
        matches!(perdedor, Err(ServerError::Conflict { .. })),
        "el perdedor tiene que recibir Conflict, nunca un 500: {perdedor:?}"
    );

    let filas: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM revisions WHERE vault_id = ? AND rev = 1",
    )
    .bind(context.vault_id)
    .fetch_one(&context.pool)
    .await?;
    assert_eq!(filas, 1, "el perdedor no puede dejar filas a medias");

    Ok(())
}

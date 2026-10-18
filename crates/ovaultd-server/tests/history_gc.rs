//! GC de blobs huérfanos (fase 5, paso 5 del plan): el invariante de `blobs.refs`
//! (revisiones con blob + manifests de snapshots), la gracia para blobs recién
//! publicados, la exclusión de uploads en curso, el borrado real (fila + CAS) y la
//! auditoría `--verify` en ambos sentidos.

mod common;

use common::{TestContext, add_op, hex_lower};
use ovaultd_protocol::BlobId;
use ovaultd_server::{history, maintenance::gc};
use sha2::{Digest, Sha256};

/// Vence la gracia sin esperar 24 h: mueve `created_at` al pasado.
async fn envejecer(
    ctx: &TestContext,
    hash: &[u8],
    minutos: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("UPDATE blobs SET created_at = datetime('now', ?) WHERE vault_id = ? AND hash = ?")
        .bind(format!("-{minutos} minutes"))
        .bind(ctx.vault_id)
        .bind(hash)
        .execute(&ctx.pool)
        .await?;
    Ok(())
}

async fn refs_de(ctx: &TestContext, hash: &[u8]) -> Result<i64, Box<dyn std::error::Error>> {
    let refs = sqlx::query_scalar("SELECT refs FROM blobs WHERE vault_id = ? AND hash = ?")
        .bind(ctx.vault_id)
        .bind(hash)
        .fetch_one(&ctx.pool)
        .await?;
    Ok(refs)
}

#[tokio::test]
async fn huerfano_viejo_es_candidato_y_el_borrado_limpia_fila_y_cas(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    // El CAS exige que el contenido corresponda al hash: se publica con el sha REAL.
    let contenido = b"contenido huerfano";
    let hash = BlobId::new(Sha256::digest(contenido).into());
    sqlx::query("INSERT INTO blobs (vault_id, hash, size) VALUES (?, ?, ?)")
        .bind(ctx.vault_id)
        .bind(hash.as_bytes().as_slice())
        .bind(contenido.len() as i64)
        .execute(&ctx.pool)
        .await?;
    ctx.cas.publish(&hash, contenido).await?;
    envejecer(&ctx, hash.as_bytes(), gc::GRACIA_MINUTOS_POR_DEFECTO + 60).await?;

    let plan = gc::planificar(&ctx.pool, None, gc::GRACIA_MINUTOS_POR_DEFECTO).await?;
    assert_eq!(plan.len(), 1, "el huérfano viejo es candidato");
    assert_eq!(plan[0].hash_hex(), hex_lower(hash.as_bytes()));
    assert_eq!(plan[0].size, contenido.len() as i64);

    let tamaño = contenido.len() as i64;
    let (borrados, bytes) = gc::borrar(&ctx.pool, &ctx.cas, &plan).await?;
    assert_eq!((borrados, bytes), (1, tamaño));

    let filas: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blobs WHERE vault_id = ?")
        .bind(ctx.vault_id)
        .fetch_one(&ctx.pool)
        .await?;
    assert_eq!(filas, 0, "la fila se fue");
    assert!(!ctx.cas.exists(&hash), "el archivo del CAS también");

    // Idempotente: repetir el borrado no explota ni borra de nuevo.
    let (borrados, _) = gc::borrar(&ctx.pool, &ctx.cas, &plan).await?;
    assert_eq!(borrados, 0, "el segundo pase no encuentra nada");
    Ok(())
}

#[tokio::test]
async fn blob_recien_publicado_no_es_candidato_por_la_gracia(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    ctx.registrar_blob(1, 10).await?; // refs = 0, pero recién creado

    let plan = gc::planificar(&ctx.pool, None, gc::GRACIA_MINUTOS_POR_DEFECTO).await?;
    assert!(plan.is_empty(), "la gracia protege al blob en vuelo");
    Ok(())
}

#[tokio::test]
async fn una_revision_retiene_al_blob_para_siempre() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    ctx.registrar_blob(1, 10).await?;
    ctx.commit(vec![add_op(1, 1, 10)], 0).await?;
    envejecer(&ctx, &[1u8; 32], gc::GRACIA_MINUTOS_POR_DEFECTO + 60).await?;

    assert_eq!(refs_de(&ctx, &[1u8; 32]).await?, 1, "el commit suma 1 (la revisión)");
    let plan = gc::planificar(&ctx.pool, None, gc::GRACIA_MINUTOS_POR_DEFECTO).await?;
    assert!(plan.is_empty(), "la historia retiene el blob, por viejo que sea");

    let discrepancias = gc::verificar(&ctx.pool, None).await?;
    assert!(discrepancias.is_empty(), "y el invariante está sano: {discrepancias:?}");
    Ok(())
}

#[tokio::test]
async fn snapshot_only_reference_survives_gc() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    ctx.registrar_blob(1, 10).await?;
    ctx.commit(vec![add_op(1, 1, 10)], 0).await?;

    // El snapshot agrega su manifest a las referencias (misma transacción).
    history::crear_snapshot(&ctx.pool, ctx.vault_id, Some(1), Some("prueba".to_string()))
        .await?;
    assert_eq!(refs_de(&ctx, &[1u8; 32]).await?, 2, "revisión + manifest");
    envejecer(&ctx, &[1u8; 32], gc::GRACIA_MINUTOS_POR_DEFECTO + 60).await?;

    let plan = gc::planificar(&ctx.pool, None, gc::GRACIA_MINUTOS_POR_DEFECTO).await?;
    assert!(plan.is_empty(), "el snapshot solo ya retiene el blob");

    // Y borrar el snapshot DEVUELVE la referencia: recién ahí el blob (sin historia que
    // lo sostenga... acá la revisión sigue, así que jamás queda huérfano) baja a 1.
    let snapshots = history::listar_snapshots(&ctx.pool, ctx.vault_id).await?;
    assert!(history::borrar_snapshot(&ctx.pool, ctx.vault_id, snapshots[0].id).await?);
    assert_eq!(refs_de(&ctx, &[1u8; 32]).await?, 1, "la revisión queda: el blob sigue vivo");

    let discrepancias = gc::verificar(&ctx.pool, None).await?;
    assert!(discrepancias.is_empty(), "invariante sano: {discrepancias:?}");
    Ok(())
}

#[tokio::test]
async fn el_borrado_excluye_uploads_en_curso() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    ctx.registrar_blob(1, 100).await?;
    ctx.registrar_blob(2, 200).await?;
    envejecer(&ctx, &[1u8; 32], gc::GRACIA_MINUTOS_POR_DEFECTO + 60).await?;
    envejecer(&ctx, &[2u8; 32], gc::GRACIA_MINUTOS_POR_DEFECTO + 60).await?;

    // Upload a medias del blob 1: declarado (total 100) y confirmado solo 40.
    sqlx::query(
        "INSERT INTO uploads (upload_id, vault_id, hash, total, offset, expires_at)
         VALUES ('u-1', ?, ?, 100, 40, datetime('now', '+1 hour'))",
    )
    .bind(ctx.vault_id)
    .bind(&[1u8; 32][..])
    .execute(&ctx.pool)
    .await?;

    let plan = gc::planificar(&ctx.pool, None, gc::GRACIA_MINUTOS_POR_DEFECTO).await?;
    assert_eq!(plan.len(), 2, "los dos son candidatos por índice");

    let (borrados, bytes) = gc::borrar(&ctx.pool, &ctx.cas, &plan).await?;
    assert_eq!((borrados, bytes), (1, 200), "el del upload en curso se saltea");

    let quedan: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blobs WHERE vault_id = ?")
        .bind(ctx.vault_id)
        .fetch_one(&ctx.pool)
        .await?;
    assert_eq!(quedan, 1, "solo el que tiene upload a medias sobrevive");
    Ok(())
}

#[tokio::test]
async fn verify_detecta_drift_y_referencias_colgadas() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    ctx.registrar_blob(1, 10).await?;
    ctx.registrar_blob(2, 10).await?;
    ctx.commit(vec![add_op(1, 1, 10)], 0).await?;

    let sanas = gc::verificar(&ctx.pool, None).await?;
    assert!(sanas.is_empty(), "arranca sano: {sanas:?}");

    // (a) Drift: el contador miente.
    sqlx::query("UPDATE blobs SET refs = 99 WHERE vault_id = ? AND hash = ?")
        .bind(ctx.vault_id)
        .bind(&[1u8; 32][..])
        .execute(&ctx.pool)
        .await?;
    // (b) Referencia colgada: un snapshot apunta a un blob sin fila.
    sqlx::query(
        "INSERT INTO snapshots (vault_id, rev, label, manifest)
         VALUES (?, 1, 'colgado', ?)",
    )
    .bind(ctx.vault_id)
    .bind(format!("{{\"path\":\"{}\"}}", hex_lower(&[2u8; 32])))
    .execute(&ctx.pool)
    .await?;

    let discrepancias = gc::verificar(&ctx.pool, None).await?;
    assert_eq!(discrepancias.len(), 2, "drift + colgada: {discrepancias:?}");

    let drift = discrepancias
        .iter()
        .find(|d| d.hash == [1u8; 32])
        .ok_or("falta la discrepancia de drift")?;
    assert_eq!((drift.refs, drift.esperado), (99, 1));

    let colgada = discrepancias
        .iter()
        .find(|d| d.hash == [2u8; 32])
        .ok_or("falta la referencia colgada")?;
    assert_eq!((colgada.refs, colgada.esperado), (0, 1));
    Ok(())
}

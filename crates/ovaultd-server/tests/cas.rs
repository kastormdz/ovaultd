//! Integración del CAS y de la transacción de commit alrededor de los blobs.
//!
//! El invariante central: el commit rechaza referencias a contenido que no está (publicar bytes
//! no alcanza si el índice no tiene la fila), y borrar + re-crear con el MISMO contenido no es
//! un no-op.

mod common;

use common::{add_op, del_op, TestContext};
use ovaultd_crypto::blob_id;
use ovaultd_protocol::BlobId;
use ovaultd_server::{storage::CasStore, ServerError};
use std::path::{Path, PathBuf};

fn archivos(dir: &Path) -> Vec<PathBuf> {
    let mut todos = Vec::new();
    let mut pendientes = vec![dir.to_path_buf()];

    while let Some(actual) = pendientes.pop() {
        for entrada in std::fs::read_dir(&actual).expect("directorio legible") {
            let entrada = entrada.expect("entrada legible");
            let ruta = entrada.path();
            if ruta.is_dir() {
                pendientes.push(ruta);
            } else {
                todos.push(ruta);
            }
        }
    }

    todos
}

#[tokio::test]
async fn publicar_y_leer_ida_y_vuelta() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let cas = CasStore::new(dir.path().to_path_buf())?;

    let bytes = b"contenido del blob";
    let id = BlobId::new(blob_id(bytes));

    cas.publish(&id, bytes).await?;

    assert!(cas.exists(&id));
    assert_eq!(cas.read(&id).await?, bytes);

    Ok(())
}

#[tokio::test]
async fn publicar_es_idempotente_y_no_deja_temporales() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let cas = CasStore::new(dir.path().to_path_buf())?;

    let bytes = b"el mismo contenido, dos veces";
    let id = BlobId::new(blob_id(bytes));

    cas.publish(&id, bytes).await?;
    cas.publish(&id, bytes).await?;

    assert_eq!(cas.read(&id).await?, bytes);

    let archivos = archivos(dir.path());
    assert_eq!(archivos.len(), 1, "un solo blob, sin copias: {archivos:?}");
    for archivo in &archivos {
        let nombre = archivo.file_name().unwrap_or_default().to_string_lossy();
        assert!(
            !nombre.starts_with(".tmp-"),
            "publicar no deja temporales: {nombre}"
        );
    }

    Ok(())
}

#[tokio::test]
async fn publicar_con_hash_ajeno_falla_y_no_escribe() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let cas = CasStore::new(dir.path().to_path_buf())?;

    let ajeno = BlobId::new([0u8; 32]);
    assert!(cas.publish(&ajeno, b"contenido que no hashea a eso").await.is_err());
    assert!(!cas.exists(&ajeno), "un hash que no coincide no publica nada");

    Ok(())
}

#[tokio::test]
async fn leer_un_blob_inexistente_falla() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let cas = CasStore::new(dir.path().to_path_buf())?;

    assert!(cas.read(&BlobId::new([7u8; 32])).await.is_err());

    Ok(())
}

#[tokio::test]
async fn referenciar_un_blob_sin_registrar_da_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;

    // El blob NO se registra: el commit tiene que negarse y no escribir nada.
    let resultado = ctx.commit(vec![add_op(1, 9, 10)], 0).await;

    match resultado {
        Err(ServerError::NotFound) => {}
        otro => panic!("se esperaba NotFound, vino {otro:?}"),
    }

    let revisions: i64 = sqlx::query_scalar("SELECT count(*) FROM revisions")
        .fetch_one(&ctx.pool)
        .await?;
    let batches: i64 = sqlx::query_scalar("SELECT count(*) FROM batches")
        .fetch_one(&ctx.pool)
        .await?;
    assert_eq!(revisions, 0);
    assert_eq!(batches, 0);

    Ok(())
}

/// El obligatorio del plan: borrar y re-crear con el MISMO blob NO es un no-op.
///
/// El dedupe del CAS es por contenido; la operación del cliente, no. La segunda `add` del mismo
/// path con el mismo blob tiene que crear una revisión nueva y revivir la fila, no "ya estaba".
#[tokio::test]
async fn restore_deleted_same_blob_is_not_noop() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    ctx.registrar_blob(9, 10).await?;

    let primera = ctx.commit(vec![add_op(1, 9, 10)], 0).await?;
    assert_eq!(primera.rev, 1);
    assert!(!primera.idempotente);

    let borrado = ctx.commit(vec![del_op(1)], 1).await?;
    assert_eq!(borrado.rev, 2);

    // La restauración: mismo path, mismo contenido (mismo blob_id), base rev 2.
    let restaurado = ctx.commit(vec![add_op(1, 9, 10)], 2).await?;
    assert_eq!(restaurado.rev, 3, "el mismo blob después de borrar NO es no-op");
    assert!(!restaurado.idempotente);

    let filas: i64 = sqlx::query_scalar("SELECT count(*) FROM revisions WHERE rev = 3")
        .fetch_one(&ctx.pool)
        .await?;
    assert_eq!(filas, 1);

    let (deleted, rev): (i64, i64) =
        sqlx::query_as("SELECT deleted, rev FROM files WHERE path_id = ?")
            .bind(vec![1u8; 32])
            .fetch_one(&ctx.pool)
            .await?;
    assert_eq!(deleted, 0, "el archivo revive");
    assert_eq!(rev, 3);

    let batches: i64 = sqlx::query_scalar("SELECT count(*) FROM batches")
        .fetch_one(&ctx.pool)
        .await?;
    assert_eq!(batches, 3);

    Ok(())
}

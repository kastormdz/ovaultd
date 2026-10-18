//! Historia por archivo (`history::listar`) y snapshots etiquetados (fase 5, paso 1):
//! la cadena de renames, la paginación hacia atrás, el manifest del snapshot y la
//! contabilidad de referencias (`blobs.refs`) que el GC de la fase 5 va a respetar.

mod common;

use axum::{
    body::Body,
    http::{HeaderValue, Method, Request, StatusCode, header::AUTHORIZATION},
};
use common::{TestContext, add_op, del_op, hex_lower, response_json};
use ovaultd_protocol::{BatchOp, Op, PathId};
use ovaultd_server::history;

/// Un `mod` es un `add` con otra op: la forma del op en el wire es la misma.
fn mod_op(path: u8, blob: u8, size: u64) -> BatchOp {
    let mut op = add_op(path, blob, size);
    op.op = Op::Mod;
    op
}

/// Un `move` publica el path NUEVO y guarda el viejo en `prev_path_id`
/// (la metadata histórica con la que la historia sigue renames).
fn move_op(nuevo: u8, viejo: u8, blob: u8, size: u64) -> BatchOp {
    let mut op = add_op(nuevo, blob, size);
    op.op = Op::Move;
    op.prev_path_id = Some(PathId::new([viejo; 32]));
    op
}

async fn refs_de(ctx: &TestContext, blob: u8) -> Result<i64, Box<dyn std::error::Error>> {
    let refs = sqlx::query_scalar("SELECT refs FROM blobs WHERE vault_id = ? AND hash = ?")
        .bind(ctx.vault_id)
        .bind(&[blob; 32][..])
        .fetch_one(&ctx.pool)
        .await?;
    Ok(refs)
}

#[tokio::test]
async fn historia_lista_las_ops_del_path_nueva_primero() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    ctx.registrar_blob(1, 10).await?;
    ctx.registrar_blob(2, 20).await?;

    ctx.commit(vec![add_op(1, 1, 10)], 0).await?;
    ctx.commit(vec![mod_op(1, 2, 20)], 1).await?;
    ctx.commit(vec![del_op(1)], 2).await?;

    let historial = history::listar(&ctx.pool, ctx.vault_id, &[1u8; 32], None, 50).await?;

    assert_eq!(historial.items.len(), 3, "add + mod + del");
    assert_eq!(historial.items[0].rev, 3);
    assert_eq!(historial.items[0].op, "del");
    assert!(
        historial.items[0].blob_hash.is_none(),
        "un del no tiene blob"
    );
    assert_eq!(historial.items[1].rev, 2);
    assert_eq!(historial.items[1].op, "mod");
    assert_eq!(
        historial.items[1].blob_hash.as_deref(),
        Some(hex_lower(&[2u8; 32]).as_str())
    );
    assert_eq!(historial.items[1].size, Some(20));
    assert_eq!(historial.items[2].rev, 1);
    assert_eq!(historial.items[2].op, "add");
    assert_eq!(historial.next_before, None, "sin más páginas");
    assert_eq!(historial.min_rev, 0);

    Ok(())
}

#[tokio::test]
async fn historia_sigue_la_cadena_de_renames() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    for blob in 1..=4u8 {
        ctx.registrar_blob(blob, 10).await?;
    }

    // A=10: add (1), mod (2). Move A→B=11 (3). mod en B (4).
    ctx.commit(vec![add_op(10, 1, 10)], 0).await?;
    ctx.commit(vec![mod_op(10, 2, 10)], 1).await?;
    ctx.commit(vec![move_op(11, 10, 3, 10)], 2).await?;
    ctx.commit(vec![mod_op(11, 4, 10)], 3).await?;

    let historial = history::listar(&ctx.pool, ctx.vault_id, &[11u8; 32], None, 50).await?;

    assert_eq!(historial.items.len(), 4, "la historia cruza el rename");
    assert_eq!(historial.items[0].rev, 4);
    assert_eq!(historial.items[0].op, "mod");
    assert_eq!(historial.items[1].rev, 3);
    assert_eq!(historial.items[1].op, "move");
    assert_eq!(
        historial.items[1].prev_path_id.as_deref(),
        Some(hex_lower(&[10u8; 32]).as_str()),
        "el move recuerda el path viejo"
    );
    assert_eq!(historial.items[2].rev, 2);
    assert_eq!(
        historial.items[2].path_id,
        hex_lower(&[10u8; 32]),
        "la edición previa al rename vive bajo el path viejo"
    );
    assert_eq!(historial.items[3].rev, 1);

    // La historia del path VIEJO sigue siendo la suya (sin lo que pasó después del move).
    let viejo = history::listar(&ctx.pool, ctx.vault_id, &[10u8; 32], None, 50).await?;
    assert_eq!(viejo.items.len(), 2);

    Ok(())
}

#[tokio::test]
async fn historia_pagina_hacia_atras() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    for blob in 1..=5u8 {
        ctx.registrar_blob(blob, 10).await?;
    }

    ctx.commit(vec![add_op(1, 1, 10)], 0).await?;
    for (blob, base) in [(2u8, 1u64), (3, 2), (4, 3), (5, 4)] {
        ctx.commit(vec![mod_op(1, blob, 10)], base).await?;
    }

    // Página 1: las dos más nuevas, con next_before para seguir.
    let pagina = history::listar(&ctx.pool, ctx.vault_id, &[1u8; 32], None, 2).await?;
    assert_eq!(pagina.items.len(), 2);
    assert_eq!(pagina.items[0].rev, 5);
    assert_eq!(pagina.items[1].rev, 4);
    assert_eq!(pagina.next_before, Some(4));

    // Página 2.
    let pagina = history::listar(&ctx.pool, ctx.vault_id, &[1u8; 32], Some(4), 2).await?;
    assert_eq!(pagina.items.len(), 2);
    assert_eq!(pagina.items[0].rev, 3);
    assert_eq!(pagina.next_before, Some(2));

    // Página 3: la última, sin más.
    let pagina = history::listar(&ctx.pool, ctx.vault_id, &[1u8; 32], Some(2), 2).await?;
    assert_eq!(pagina.items.len(), 1);
    assert_eq!(pagina.items[0].rev, 1);
    assert_eq!(pagina.next_before, None);

    Ok(())
}

#[tokio::test]
async fn snapshot_crea_manifest_y_ajusta_refs() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    ctx.registrar_blob(1, 10).await?;
    ctx.registrar_blob(2, 10).await?;

    ctx.commit(vec![add_op(1, 1, 10)], 0).await?;
    ctx.commit(vec![add_op(2, 2, 10)], 1).await?;

    let base_uno = refs_de(&ctx, 1).await?;
    let base_dos = refs_de(&ctx, 2).await?;

    let snapshot = history::crear_snapshot(
        &ctx.pool,
        ctx.vault_id,
        None,
        Some("antes de reorganizar".to_owned()),
    )
    .await?;

    assert_eq!(snapshot.rev, 2, "el corte default es el head");
    assert_eq!(snapshot.manifest.len(), 2, "los dos files vivos");
    assert_eq!(
        snapshot.manifest.get(hex_lower(&[1u8; 32]).as_str()),
        Some(&hex_lower(&[1u8; 32]))
    );
    assert_eq!(
        refs_de(&ctx, 1).await?,
        base_uno + 1,
        "el manifest cuenta como referencia viva"
    );
    assert_eq!(refs_de(&ctx, 2).await?, base_dos + 1);

    // El listado lo ve y el detalle devuelve el mismo manifest.
    let lista = history::listar_snapshots(&ctx.pool, ctx.vault_id).await?;
    assert_eq!(lista.len(), 1);
    let detalle = history::obtener_snapshot(&ctx.pool, ctx.vault_id, snapshot.id).await?;
    assert_eq!(detalle.manifest, snapshot.manifest);

    // Borrarlo devuelve las referencias (si no, el GC nunca podría reclamar el espacio).
    assert!(history::borrar_snapshot(&ctx.pool, ctx.vault_id, snapshot.id).await?);
    assert_eq!(refs_de(&ctx, 1).await?, base_uno);
    assert_eq!(refs_de(&ctx, 2).await?, base_dos);
    assert!(
        history::listar_snapshots(&ctx.pool, ctx.vault_id)
            .await?
            .is_empty()
    );
    assert!(
        !history::borrar_snapshot(&ctx.pool, ctx.vault_id, snapshot.id).await?,
        "borrar dos veces no existe"
    );

    Ok(())
}

#[tokio::test]
async fn snapshot_en_rev_vieja_solo_incluye_lo_vivo_ahi() -> Result<(), Box<dyn std::error::Error>>
{
    let ctx = TestContext::new().await?;
    ctx.registrar_blob(1, 10).await?;
    ctx.registrar_blob(2, 10).await?;

    ctx.commit(vec![add_op(1, 1, 10)], 0).await?;
    ctx.commit(vec![add_op(2, 2, 10)], 1).await?;

    // Un corte en la rev 1 solo ve el primer archivo.
    let snapshot = history::crear_snapshot(&ctx.pool, ctx.vault_id, Some(1), None).await?;
    assert_eq!(snapshot.rev, 1);
    assert_eq!(snapshot.manifest.len(), 1);
    assert!(
        snapshot
            .manifest
            .contains_key(hex_lower(&[1u8; 32]).as_str())
    );

    // Pedir un rev futuro se clampea al head (no hay historia que inventar).
    let futuro = history::crear_snapshot(&ctx.pool, ctx.vault_id, Some(99), None).await?;
    assert_eq!(futuro.rev, 2);

    Ok(())
}

#[tokio::test]
async fn history_http_valida_token_y_path_id() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    ctx.registrar_blob(1, 10).await?;
    ctx.commit(vec![add_op(1, 1, 10)], 0).await?;

    let path_hex = hex_lower(&[1u8; 32]);
    let uri = format!("/api/v1/vaults/{}/history?path_id={path_hex}", ctx.vault_id);

    // Con token: 200 y la historia.
    let response = ctx
        .authenticated_request(Method::GET, &uri, Body::empty())
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let cuerpo = response_json(response).await?;
    let items = cuerpo["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["op"], "add");
    assert_eq!(items[0]["path_id"], path_hex);

    // Sin token: 401.
    let anonimo = ctx
        .request(
            Request::builder()
                .method(Method::GET)
                .uri(&uri)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(anonimo.status(), StatusCode::UNAUTHORIZED);

    // path_id con forma inválida: 400.
    let malo = ctx
        .authenticated_request(
            Method::GET,
            &format!("/api/v1/vaults/{}/history?path_id=xyz", ctx.vault_id),
            Body::empty(),
        )
        .await?;
    assert_eq!(malo.status(), StatusCode::BAD_REQUEST);

    // Snapshot por HTTP: 201 con manifest.
    let snap_uri = format!("/api/v1/vaults/{}/snapshots", ctx.vault_id);
    let response = ctx
        .request(
            Request::builder()
                .method(Method::POST)
                .uri(&snap_uri)
                .header(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", ctx.token))?,
                )
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"label":"http"}"#))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    let snapshot = response_json(response).await?;
    assert_eq!(snapshot["label"], "http");
    assert_eq!(snapshot["manifest"].as_object().map(|m| m.len()), Some(1));

    // Y el listado también responde.
    let lista = ctx
        .authenticated_request(Method::GET, &snap_uri, Body::empty())
        .await?;
    assert_eq!(lista.status(), StatusCode::OK);

    Ok(())
}

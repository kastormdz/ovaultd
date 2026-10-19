//! Regresión de la segunda auditoría (2026-10-19). Cada test corresponde a un hallazgo y
//! debe morir si el fix se revierte:
//!
//! - F-01: el GC no borra del CAS un blob que OTRO vault sigue referenciando.
//! - F-02: `changes` no carga el oplog entero (LIMIT real + `limit = 0` clampeado).
//! - F-03: los snapshots tienen tope por vault.
//! - F-06: Argon2id con parámetros explícitos y política de 12 (+ lista de comunes).
//! - F-08: los nombres de device tienen tope y no aceptan saltos de línea.
//! - F-10: el rol del vault sale de `vault_members` (fallback `owner`).

mod common;

use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, Method, Request, StatusCode},
};
use common::{add_op, TestContext};
use ovaultd_protocol::BlobId;
use ovaultd_server::{auth, delta, history, maintenance::gc};
use sha2::{Digest, Sha256};

/// F-01: dos filas `blobs` con el mismo hash (una viva, otra huérfana) y UN archivo en el
/// CAS. Ni el plan ni el borrado pueden tocar los bytes: el vault sano quedaría en 404.
#[tokio::test]
async fn f01_el_gc_no_borra_del_cas_un_blob_que_otro_vault_referencia(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let otro_vault = ctx.agregar_vault_propio("segundo-vault").await?;

    let contenido = b"plantilla identica en dos vaults";
    let hash = BlobId::new(Sha256::digest(contenido).into());
    ctx.cas.publish(&hash, contenido).await?;

    // Vault del contexto: fila VIVA. Vault nuevo: misma hash, huérfana y vieja.
    for (vault, refs) in [(ctx.vault_id, 1_i64), (otro_vault, 0_i64)] {
        sqlx::query(
            "INSERT INTO blobs (vault_id, hash, size, refs, created_at)
             VALUES (?, ?, ?, ?, datetime('now', '-2 days'))",
        )
        .bind(vault)
        .bind(hash.as_bytes().as_slice())
        .bind(contenido.len() as i64)
        .bind(refs)
        .execute(&ctx.pool)
        .await?;
    }

    let plan = gc::planificar(&ctx.pool, None, gc::GRACIA_MINUTOS_POR_DEFECTO).await?;
    assert!(
        plan.is_empty(),
        "un hash que otro vault referencia no puede ser candidato (CAS global)"
    );

    // Defensa en profundidad: aunque el plan envejezca, `borrar` re-verifica en su tx.
    let forzado = vec![gc::Candidato {
        vault_id: otro_vault,
        hash: hash.as_bytes().to_vec(),
        size: contenido.len() as i64,
        created_at: "2026-01-01 00:00:00".to_owned(),
    }];
    let (borrados, _bytes) = gc::borrar(&ctx.pool, &ctx.cas, &forzado).await?;
    assert_eq!(borrados, 0, "no se borra del CAS un blob compartido");

    let leidos = ctx.cas.read(&hash).await?;
    assert_eq!(leidos, contenido, "los bytes del vault sano siguen ahí");
    Ok(())
}

/// F-02: con 20 revisiones, `limit = 1` devuelve UNA y `limit = 0` se clampea a 1 (antes
/// traía el oplog entero en memoria).
#[tokio::test]
async fn f02_changes_no_trae_el_oplog_entero(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    for i in 1..=20_u8 {
        // El commit exige el blob registrado para ESTE vault (como una subida real).
        ctx.registrar_blob(i, 10).await?;
        ctx.commit(vec![add_op(i, i, 10)], u64::from(i) - 1).await?;
    }

    let uno = delta::changes(&ctx.pool, ctx.vault_id, 0, 1).await?;
    assert_eq!(uno.items.len(), 1, "limit = 1 devuelve un solo registro");
    assert_eq!(uno.next_since, 1);

    let cero = delta::changes(&ctx.pool, ctx.vault_id, 0, 0).await?;
    assert_eq!(
        cero.items.len(),
        1,
        "limit = 0 ya no significa 'traeme el oplog entero'"
    );
    Ok(())
}

/// F-03: el snapshot 101 se rechaza; los 100 primeros entran.
#[tokio::test]
async fn f03_los_snapshots_tienen_tope_por_vault() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    for _ in 0..100 {
        history::crear_snapshot(&ctx.pool, ctx.vault_id, None, None).await?;
    }

    let error = history::crear_snapshot(&ctx.pool, ctx.vault_id, None, None).await;
    assert!(error.is_err(), "el snapshot 101 debe rechazarse");

    let etiqueta_larga = "x".repeat(200);
    let error = history::crear_snapshot(&ctx.pool, ctx.vault_id, None, Some(etiqueta_larga)).await;
    assert!(error.is_err(), "la etiqueta también tiene tope");
    Ok(())
}

/// F-06: parámetros explícitos (m=64 MiB, t=3, p=4) y política de 12 (+ comunes).
///
/// La compatibilidad con los PHC viejos es por diseño (`verificar_password` verifica con
/// los parámetros que trae el propio hash) y se comprueba en el smoke contra la cuenta
/// real, que ya tiene un hash de los parámetros anteriores.
#[test]
fn f06_argon2_explicito_y_politica_de_12() {
    let phc = auth::hash_password("Correcta-Horse-9").expect("hash");
    assert!(phc.contains("m=65536,t=3,p=4"), "PHC con params explícitos: {phc}");
    assert!(auth::verificar_password("Correcta-Horse-9", &phc).expect("verify"));
    assert!(!auth::verificar_password("Otra-Cosa-123", &phc).expect("verify"));

    assert!(auth::password_debil("password123"), "11 caracteres");
    assert!(auth::password_debil("password1234"), "está en la lista de comunes");
    assert!(!auth::password_debil("Correcta-Horse-9"));
}

/// F-08: nombre de 200 KB o con `\n` → 400.
#[tokio::test]
async fn f08_los_nombres_de_device_tienen_tope() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;

    for nombre in ["x".repeat(200_000), "ok\nfalso log".to_owned()] {
        let cuerpo = serde_json::json!({ "name": nombre }).to_string();
        let pedido = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/devices")
            .header(CONTENT_TYPE, "application/json")
            .header(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {}", ctx.token),
            )
            .body(Body::from(cuerpo))?;
        let respuesta = ctx.request(pedido).await?;
        assert_eq!(respuesta.status(), StatusCode::BAD_REQUEST, "nombre inválido");
    }
    Ok(())
}

/// F-10: el listado de vaults devuelve el rol real (sin miembros → `owner`).
#[tokio::test]
async fn f10_el_rol_sale_de_vault_members() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let respuesta = ctx
        .authenticated_request(Method::GET, "/api/v1/vaults", Body::empty())
        .await?;
    assert_eq!(respuesta.status(), StatusCode::OK);
    let vaults = common::response_json(respuesta).await?;
    let rol = vaults[0]["role"].as_str().unwrap_or_default();
    assert_eq!(rol, "owner", "sin miembros, el dueño sigue siendo owner");
    Ok(())
}

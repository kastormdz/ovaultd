//! Borrado de vaults (confirmación fuerte por nombre + cascada + barrido del CAS + auditoría)
//! y actualización del material (re-sellado de la envuelta de la password).

mod common;

use axum::{
    body::Body,
    http::{
        header::AUTHORIZATION, header::CONTENT_TYPE, HeaderValue, Method, Request, StatusCode,
    },
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use common::{hex_lower, response_json, TestContext};
use ovaultd_protocol::BlobId;
use sha2::{Digest, Sha256};

/// Sube un blob de prueba al vault del contexto y devuelve su digest.
async fn subir_blob(
    ctx: &TestContext,
    bytes: &[u8],
) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let uri = format!(
        "/api/v1/vaults/{}/blobs/{}",
        ctx.vault_id,
        hex_lower(&digest)
    );

    let respuesta = ctx
        .authenticated_request(Method::PUT, &uri, Body::from(bytes.to_vec()))
        .await?;
    assert_eq!(respuesta.status(), StatusCode::CREATED, "PUT del blob");

    Ok(digest)
}

fn pedido_json(
    token: &str,
    method: Method,
    uri: &str,
    cuerpo: &serde_json::Value,
) -> Result<Request<Body>, Box<dyn std::error::Error>> {
    Ok(Request::builder()
        .method(method)
        .uri(uri.to_owned())
        .header(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}"))?)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(cuerpo.to_string()))?)
}

#[tokio::test]
async fn borrar_vault_exige_el_nombre_y_limpia_cascada_y_cas(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let digest = subir_blob(&ctx, b"contenido efimero del vault a borrar").await?;
    assert!(
        ctx.cas.exists(&BlobId::new(digest)),
        "el blob quedó publicado en el CAS"
    );

    let uri = format!("/api/v1/vaults/{}", ctx.vault_id);

    // Sin confirmación y con una equivocada: 400 y el vault sigue intacto.
    let sin = ctx
        .authenticated_request(Method::DELETE, &uri, Body::empty())
        .await?;
    assert_eq!(sin.status(), StatusCode::BAD_REQUEST);
    let mal = ctx
        .authenticated_request(Method::DELETE, &format!("{uri}?confirm=otro"), Body::empty())
        .await?;
    assert_eq!(mal.status(), StatusCode::BAD_REQUEST);

    let nombre: String = sqlx::query_scalar("SELECT name FROM vaults WHERE id = ?")
        .bind(ctx.vault_id)
        .fetch_one(&ctx.pool)
        .await?;
    assert_eq!(nombre, "vault-de-integracion");

    // Con el nombre exacto: 204.
    let ok = ctx
        .authenticated_request(
            Method::DELETE,
            &format!("{uri}?confirm={nombre}"),
            Body::empty(),
        )
        .await?;
    assert_eq!(ok.status(), StatusCode::NO_CONTENT);

    // La cascada deja TODAS las tablas hijas en cero.
    for (tabla, consulta) in [
        ("vaults", "SELECT count(*) FROM vaults WHERE id = ?"),
        (
            "revisions",
            "SELECT count(*) FROM revisions WHERE vault_id = ?",
        ),
        ("files", "SELECT count(*) FROM files WHERE vault_id = ?"),
        ("blobs", "SELECT count(*) FROM blobs WHERE vault_id = ?"),
        (
            "vault_members",
            "SELECT count(*) FROM vault_members WHERE vault_id = ?",
        ),
    ] {
        let restantes: i64 = sqlx::query_scalar(consulta)
            .bind(ctx.vault_id)
            .fetch_one(&ctx.pool)
            .await?;
        assert_eq!(restantes, 0, "la cascada deja {tabla} en cero");
    }

    // El barrido se llevó el blob físico del CAS.
    assert!(
        !ctx.cas.exists(&BlobId::new(digest)),
        "el barrido sacó el blob del CAS"
    );

    // Quedó auditado.
    let eventos: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'vault_delete' AND actor_account_id = ?",
    )
    .bind(ctx.account_id)
    .fetch_one(&ctx.pool)
    .await?;
    assert_eq!(eventos, 1);

    // Y el material responde 404 (el vault ya no existe).
    let material = ctx
        .authenticated_request(Method::GET, &format!("{uri}/material"), Body::empty())
        .await?;
    assert_eq!(material.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn un_vault_ajeno_es_404_para_el_token_propio_y_el_dueno_si_puede(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let (ajeno_id, token_ajeno) = ctx.agregar_vault_ajeno("otro@ejemplo.com").await?;

    // El token de la cuenta A no puede borrar el vault de la cuenta B (404, ni filtra).
    let uri = format!("/api/v1/vaults/{ajeno_id}?confirm=vault-ajeno");
    let intromision = ctx
        .authenticated_request(Method::DELETE, &uri, Body::empty())
        .await?;
    assert_eq!(intromision.status(), StatusCode::NOT_FOUND);

    let sigue: i64 = sqlx::query_scalar("SELECT count(*) FROM vaults WHERE id = ?")
        .bind(ajeno_id)
        .fetch_one(&ctx.pool)
        .await?;
    assert_eq!(sigue, 1, "el vault ajeno sigue existiendo");

    // El dueño, con su token y su nombre, sí puede.
    let suyo = ctx.request_con_token(
        &token_ajeno,
        Method::DELETE,
        &format!("/api/v1/vaults/{ajeno_id}?confirm=vault-ajeno"),
        Body::empty(),
    )
    .await?;
    assert_eq!(suyo.status(), StatusCode::NO_CONTENT);

    Ok(())
}

#[tokio::test]
async fn actualizar_material_guarda_la_envuelta_y_valida_la_entrada(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let uri = format!("/api/v1/vaults/{}/material", ctx.vault_id);

    // Una envuelta "válida" (el server no puede abrirla: E2EE) de 72 bytes.
    let envuelta = vec![7u8; 24 + 48];
    let cuerpo = serde_json::json!({ "wrapped_key_pw": STANDARD.encode(&envuelta) });
    let respuesta = ctx
        .request(pedido_json(&ctx.token, Method::PUT, &uri, &cuerpo)?)
        .await?;
    assert_eq!(respuesta.status(), StatusCode::OK);

    let guardada: Vec<u8> = sqlx::query_scalar("SELECT wrapped_key_pw FROM vaults WHERE id = ?")
        .bind(ctx.vault_id)
        .fetch_one(&ctx.pool)
        .await?;
    assert_eq!(guardada, envuelta, "la envuelta nueva quedó guardada");

    // Demasiado corta: 400.
    let corta = serde_json::json!({ "wrapped_key_pw": STANDARD.encode(vec![1u8; 10]) });
    let r_corta = ctx
        .request(pedido_json(&ctx.token, Method::PUT, &uri, &corta)?)
        .await?;
    assert_eq!(r_corta.status(), StatusCode::BAD_REQUEST);

    // No-base64: 400.
    let basura = serde_json::json!({ "wrapped_key_pw": "no-es-base64!!" });
    let r_basura = ctx
        .request(pedido_json(&ctx.token, Method::PUT, &uri, &basura)?)
        .await?;
    assert_eq!(r_basura.status(), StatusCode::BAD_REQUEST);

    // El GET devuelve exactamente lo nuevo (round-trip base64).
    let get = ctx
        .authenticated_request(Method::GET, &uri, Body::empty())
        .await?;
    assert_eq!(get.status(), StatusCode::OK);
    let json = response_json(get).await?;
    assert_eq!(json["wrapped_key_pw"], STANDARD.encode(&envuelta));

    // Auditado.
    let eventos: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'vault_rekey' AND actor_account_id = ?",
    )
    .bind(ctx.account_id)
    .fetch_one(&ctx.pool)
    .await?;
    assert_eq!(eventos, 1);

    Ok(())
}

//! Integración de la superficie HTTP de blobs: subida (que registra el índice), descarga con
//! Range, y el invariante cross-vault: un blob de otro vault NO se lee ni conociendo el hash.

mod common;

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, header::RANGE, HeaderValue, Method, Request, StatusCode},
};
use common::{add_op_con_blob, hex_lower, response_bytes, response_json, TestContext};
use ovaultd_protocol::BlobId;
use ovaultd_server::storage::blob_registrado;
use sha2::{Digest, Sha256};

fn hash_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

async fn subir_blob(
    ctx: &TestContext,
    bytes: &[u8],
) -> Result<String, Box<dyn std::error::Error>> {
    let hash = hash_hex(bytes);
    let uri = format!("/api/v1/vaults/{}/blobs/{hash}", ctx.vault_id);

    let response = ctx
        .authenticated_request(Method::PUT, &uri, Body::from(bytes.to_vec()))
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED, "PUT del blob");

    Ok(hash)
}

#[tokio::test]
async fn put_registra_en_el_indice_y_get_devuelve_los_bytes(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let bytes = b"contenido para ida y vuelta";
    let digest: [u8; 32] = Sha256::digest(bytes).into();

    let hash = subir_blob(&ctx, bytes).await?;

    // Sin la fila del índice, el commit rechazaría la referencia ("contenido que no está").
    let registrados: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM blobs WHERE vault_id = ? AND hash = ?",
    )
    .bind(ctx.vault_id)
    .bind(digest.as_slice())
    .fetch_one(&ctx.pool)
    .await?;
    assert_eq!(registrados, 1);
    assert!(
        blob_registrado(&ctx.pool, ctx.vault_id, &BlobId::new(digest)).await?,
        "el PUT tiene que dejar el blob registrado para el vault"
    );

    let uri = format!("/api/v1/vaults/{}/blobs/{hash}", ctx.vault_id);
    let get = ctx
        .authenticated_request(Method::GET, &uri, Body::empty())
        .await?;
    assert_eq!(get.status(), StatusCode::OK);
    assert_eq!(response_bytes(get).await?, bytes);

    Ok(())
}

#[tokio::test]
async fn put_repetido_es_idempotente() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let bytes = b"blob idempotente";
    let hash = hash_hex(bytes);
    let uri = format!("/api/v1/vaults/{}/blobs/{hash}", ctx.vault_id);

    let primero = ctx
        .authenticated_request(Method::PUT, &uri, Body::from(bytes.to_vec()))
        .await?;
    assert_eq!(primero.status(), StatusCode::CREATED);

    let segundo = ctx
        .authenticated_request(Method::PUT, &uri, Body::from(bytes.to_vec()))
        .await?;
    assert_eq!(segundo.status(), StatusCode::OK);

    let filas: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM blobs WHERE vault_id = ? AND hash = ?",
    )
    .bind(ctx.vault_id)
    .bind(Sha256::digest(bytes).as_slice())
    .fetch_one(&ctx.pool)
    .await?;
    assert_eq!(filas, 1, "la segunda subida no duplica el índice");

    Ok(())
}

#[tokio::test]
async fn put_con_hash_que_no_coincide_da_400_y_no_guarda_nada(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;

    let hash_solicitado = [0u8; 32];
    let uri = format!(
        "/api/v1/vaults/{}/blobs/{}",
        ctx.vault_id,
        hex_lower(&hash_solicitado)
    );

    let response = ctx
        .authenticated_request(
            Method::PUT,
            &uri,
            Body::from(b"contenido que no hashea a eso".to_vec()),
        )
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    assert!(!ctx.cas.exists(&BlobId::new(hash_solicitado)));

    let filas: i64 = sqlx::query_scalar("SELECT count(*) FROM blobs")
        .fetch_one(&ctx.pool)
        .await?;
    assert_eq!(filas, 0, "un hash que no coincide no deja NADA");

    Ok(())
}

#[tokio::test]
async fn get_con_range_devuelve_206_y_el_trozo() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let bytes = b"0123456789";
    let hash = subir_blob(&ctx, bytes).await?;

    let uri = format!("/api/v1/vaults/{}/blobs/{hash}", ctx.vault_id);
    let request = Request::builder()
        .method(Method::GET)
        .uri(&uri)
        .header(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", ctx.token))?,
        )
        .header(RANGE, "bytes=2-5")
        .body(Body::empty())?;

    let response = ctx.request(request).await?;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response
            .headers()
            .get("content-range")
            .and_then(|valor| valor.to_str().ok()),
        Some("bytes 2-5/10")
    );
    assert_eq!(response_bytes(response).await?, b"2345");

    Ok(())
}

/// El obligatorio del plan: un blob de otro vault **no se lee ni conociendo el hash**.
///
/// El CAS es un filesystem compartido por TODOS los vaults (un solo root por hash), así que la
/// frontera de autorización es el índice (`blobs` por vault). Sin el chequeo, este test lee
/// 200 con el contenido ajeno.
#[tokio::test]
async fn cross_vault_blob_read_is_denied() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let bytes = b"secreto que vive en el vault A";
    let hash = subir_blob(&ctx, bytes).await?;

    // Control positivo: en su propio vault se lee (el test no es vacuo).
    let uri_propio = format!("/api/v1/vaults/{}/blobs/{hash}", ctx.vault_id);
    let propio = ctx
        .authenticated_request(Method::GET, &uri_propio, Body::empty())
        .await?;
    assert_eq!(propio.status(), StatusCode::OK);
    assert_eq!(response_bytes(propio).await?, bytes);

    // (1) Mismo dueño, OTRO vault suyo: 404 — nunca 403 (no se confirma ni la existencia).
    let vault_b = ctx.agregar_vault_propio("vault-b").await?;
    let uri_b = format!("/api/v1/vaults/{vault_b}/blobs/{hash}");
    let ajeno = ctx
        .authenticated_request(Method::GET, &uri_b, Body::empty())
        .await?;
    assert_eq!(ajeno.status(), StatusCode::NOT_FOUND);
    let cuerpo = response_json(ajeno).await?;
    assert_eq!(cuerpo["code"], "not_found");

    // (2) Otra cuenta, con su propio vault: tampoco. Conoce el hash; no alcanza.
    let (vault_c, token_c) = ctx.agregar_vault_ajeno("ajeno@example.com").await?;
    let uri_c = format!("/api/v1/vaults/{vault_c}/blobs/{hash}");
    let ajeno2 = ctx
        .request_con_token(&token_c, Method::GET, &uri_c, Body::empty())
        .await?;
    assert_eq!(ajeno2.status(), StatusCode::NOT_FOUND);

    Ok(())
}

/// El flujo real entre superficies: subir por HTTP y commitear por el camino de escritura.
#[tokio::test]
async fn blob_subido_por_http_sirve_para_el_batch() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let bytes = b"ciphertext de mentira, pero consistente";
    subir_blob(&ctx, bytes).await?;

    let blob_id = BlobId::new(Sha256::digest(bytes).into());
    let size = u64::try_from(bytes.len()).expect("largos que caben");

    let outcome = ctx
        .commit(vec![add_op_con_blob(5, blob_id, size)], 0)
        .await?;
    assert_eq!(outcome.rev, 1);

    let revisiones: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM revisions WHERE vault_id = ? AND blob_hash = ?",
    )
    .bind(ctx.vault_id)
    .bind(blob_id.as_bytes().as_slice())
    .fetch_one(&ctx.pool)
    .await?;
    assert_eq!(revisiones, 1);

    Ok(())
}

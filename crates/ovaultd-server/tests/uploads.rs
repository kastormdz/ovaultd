//! Integración de las subidas reanudables (`tus` core, ADR-0012): creación, reanudación por
//! `Upload-Offset`, dedupe, excesos, hash final y las reservas de cuota.

mod common;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
    response::Response,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use common::{hex_lower, response_bytes, response_json, TestContext};
use ovaultd_protocol::BlobId;
use sha2::{Digest, Sha256};

const TUS: &str = "1.0.0";

fn blob_de(bytes: &[u8]) -> BlobId {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    BlobId::new(digest)
}

fn header_i64(response: &Response, nombre: &str) -> i64 {
    response
        .headers()
        .get(nombre)
        .and_then(|valor| valor.to_str().ok())
        .and_then(|valor| valor.parse::<i64>().ok())
        .unwrap_or(-1)
}

async fn crear_subida(
    ctx: &TestContext,
    total: u64,
    blob_id: &BlobId,
) -> Result<Response, Box<dyn std::error::Error>> {
    let metadata = format!("blob_id {}", STANDARD.encode(blob_id.as_bytes()));
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/v1/vaults/{}/blobs", ctx.vault_id))
        .header(header::AUTHORIZATION, format!("Bearer {}", ctx.token))
        .header("tus-resumable", TUS)
        .header("upload-length", total.to_string())
        .header("upload-metadata", metadata)
        .body(Body::empty())?;

    Ok(ctx.request(request).await?)
}

async fn upload_id_de(response: Response) -> Result<String, Box<dyn std::error::Error>> {
    let cuerpo = response_json(response).await?;
    Ok(cuerpo["upload_id"]
        .as_str()
        .ok_or("la respuesta de creación no trae upload_id")?
        .to_owned())
}

async fn parchear(
    ctx: &TestContext,
    upload_id: &str,
    offset: u64,
    bytes: Vec<u8>,
) -> Result<Response, Box<dyn std::error::Error>> {
    let request = Request::builder()
        .method(Method::PATCH)
        .uri(format!("/api/v1/vaults/{}/uploads/{upload_id}", ctx.vault_id))
        .header(header::AUTHORIZATION, format!("Bearer {}", ctx.token))
        .header("tus-resumable", TUS)
        .header("upload-offset", offset.to_string())
        .header(header::CONTENT_TYPE, "application/offset+octet-stream")
        .body(Body::from(bytes))?;

    Ok(ctx.request(request).await?)
}

async fn sonda(ctx: &TestContext, upload_id: &str) -> Result<Response, Box<dyn std::error::Error>> {
    let request = Request::builder()
        .method(Method::HEAD)
        .uri(format!("/api/v1/vaults/{}/uploads/{upload_id}", ctx.vault_id))
        .header(header::AUTHORIZATION, format!("Bearer {}", ctx.token))
        .header("tus-resumable", TUS)
        .body(Body::empty())?;

    Ok(ctx.request(request).await?)
}

async fn cuota_del_vault(ctx: &TestContext, cuota: i64) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("UPDATE vaults SET quota_bytes = ? WHERE id = ?")
        .bind(cuota)
        .bind(ctx.vault_id)
        .execute(&ctx.pool)
        .await?;
    Ok(())
}

async fn contenido_de_prueba(largo: usize) -> Vec<u8> {
    (0..largo as u32).map(|indice| (indice % 251) as u8).collect()
}

#[tokio::test]
async fn upload_reanudable_completa_por_chunks_y_se_reanuda() -> Result<(), Box<dyn std::error::Error>>
{
    let ctx = TestContext::new().await?;
    let bytes = contenido_de_prueba(300_000).await;
    let blob_id = blob_de(&bytes);
    let total = u64::try_from(bytes.len())?;

    let creada = crear_subida(&ctx, total, &blob_id).await?;
    assert_eq!(creada.status(), StatusCode::CREATED);
    assert!(
        creada.headers().get(header::LOCATION).is_some(),
        "la creación trae Location para reanudar"
    );
    assert_eq!(header_i64(&creada, "upload-offset"), 0);
    let upload_id = upload_id_de(creada).await?;

    // Primer bloque.
    let primero = parchear(&ctx, &upload_id, 0, bytes[..100_000].to_vec()).await?;
    assert_eq!(primero.status(), StatusCode::NO_CONTENT);
    assert_eq!(header_i64(&primero, "upload-offset"), 100_000);

    // "Se corta la conexión": HEAD dice exactamente dónde reanudar.
    let sonda = sonda(&ctx, &upload_id).await?;
    assert_eq!(sonda.status(), StatusCode::OK);
    assert_eq!(header_i64(&sonda, "upload-offset"), 100_000);
    assert_eq!(header_i64(&sonda, "upload-length"), i64::try_from(total)?);

    // El resto, llegando por donde toca.
    let segundo = parchear(&ctx, &upload_id, 100_000, bytes[100_000..200_000].to_vec()).await?;
    assert_eq!(header_i64(&segundo, "upload-offset"), 200_000);

    let tercero = parchear(&ctx, &upload_id, 200_000, bytes[200_000..].to_vec()).await?;
    assert_eq!(tercero.status(), StatusCode::NO_CONTENT);
    assert_eq!(header_i64(&tercero, "upload-offset"), i64::try_from(total)?);

    // El contenido quedó publicado y registrado: se lee por la superficie de blobs.
    let uri = format!(
        "/api/v1/vaults/{}/blobs/{}",
        ctx.vault_id,
        hex_lower(blob_id.as_bytes())
    );
    let descarga = ctx
        .authenticated_request(Method::GET, &uri, Body::empty())
        .await?;
    assert_eq!(descarga.status(), StatusCode::OK);
    assert_eq!(response_bytes(descarga).await?, bytes);

    // Y el temporal se fue: el contenido vive en el CAS.
    let temporal = ctx
        .config
        .cas_root
        .join(".uploads")
        .join(format!("{upload_id}.part"));
    assert!(!temporal.exists(), "el .part no sobrevive a la publicación");

    Ok(())
}

#[tokio::test]
async fn offset_con_hueco_da_409_y_devuelve_el_offset_actual(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let bytes = vec![7u8; 20];
    let blob_id = blob_de(&bytes);

    let creada = crear_subida(&ctx, 20, &blob_id).await?;
    let upload_id = upload_id_de(creada).await?;

    // Hueco: pide escribir en 5 cuando el confirmado sigue en 0.
    let hueco = parchear(&ctx, &upload_id, 5, vec![0u8; 5]).await?;
    assert_eq!(hueco.status(), StatusCode::CONFLICT);
    assert_eq!(
        header_i64(&hueco, "upload-offset"),
        0,
        "el 409 dice dónde reanudar de verdad"
    );
    let cuerpo = response_json(hueco).await?;
    assert_eq!(cuerpo["code"], "bad_request");

    // El bloque correcto entra sin drama.
    let correcto = parchear(&ctx, &upload_id, 0, bytes).await?;
    assert_eq!(correcto.status(), StatusCode::NO_CONTENT);
    assert_eq!(header_i64(&correcto, "upload-offset"), 20);

    Ok(())
}

#[tokio::test]
async fn retry_del_chunk_final_es_idempotente() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let bytes = contenido_de_prueba(64_000).await;
    let blob_id = blob_de(&bytes);
    let total = u64::try_from(bytes.len())?;

    let creada = crear_subida(&ctx, total, &blob_id).await?;
    let upload_id = upload_id_de(creada).await?;

    // De una sola vez.
    let completa = parchear(&ctx, &upload_id, 0, bytes.clone()).await?;
    assert_eq!(header_i64(&completa, "upload-offset"), i64::try_from(total)?);

    // Respuesta perdida: el cliente reintenta el MISMO bloque final.
    let reintento = parchear(&ctx, &upload_id, 0, bytes.clone()).await?;
    assert_eq!(reintento.status(), StatusCode::NO_CONTENT);
    assert_eq!(header_i64(&reintento, "upload-offset"), i64::try_from(total)?);

    // No se duplicó nada en el índice.
    let filas: i64 = sqlx::query_scalar("SELECT count(*) FROM blobs WHERE vault_id = ? AND hash = ?")
        .bind(ctx.vault_id)
        .bind(blob_id.as_bytes().as_slice())
        .fetch_one(&ctx.pool)
        .await?;
    assert_eq!(filas, 1);

    Ok(())
}

#[tokio::test]
async fn hash_final_que_no_coincide_da_400_y_no_publica(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;

    // Declara el hash de "otra cosa" y manda 16 bytes distintos.
    let declarado = blob_de(b"otra cosa");
    let mentira = vec![0x11u8; 16];

    let creada = crear_subida(&ctx, 16, &declarado).await?;
    let upload_id = upload_id_de(creada).await?;

    let rechazo = parchear(&ctx, &upload_id, 0, mentira).await?;
    assert_eq!(rechazo.status(), StatusCode::BAD_REQUEST);
    assert!(!ctx.cas.exists(&declarado), "no se publica lo que no hashea");

    let filas: i64 = sqlx::query_scalar("SELECT count(*) FROM blobs")
        .fetch_one(&ctx.pool)
        .await?;
    assert_eq!(filas, 0);

    // Subida muerta: el verificador fue terminal.
    let muerta = sonda(&ctx, &upload_id).await?;
    assert_eq!(muerta.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn exceso_sobre_lo_declarado_da_400_y_termina_la_subida(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let bytes = vec![0x22u8; 12];
    let blob_id = blob_de(&bytes);

    let creada = crear_subida(&ctx, 10, &blob_id).await?;
    let upload_id = upload_id_de(creada).await?;

    // Primer bloque válido: quedan 4 bytes.
    let parcial = parchear(&ctx, &upload_id, 0, vec![0x33u8; 6]).await?;
    assert_eq!(header_i64(&parcial, "upload-offset"), 6);

    // Con offset correcto, el bloque se pasa de lo declarado: terminal.
    let exceso = parchear(&ctx, &upload_id, 6, vec![0x44u8; 6]).await?;
    assert_eq!(exceso.status(), StatusCode::BAD_REQUEST);

    let muerta = sonda(&ctx, &upload_id).await?;
    assert_eq!(
        muerta.status(),
        StatusCode::NOT_FOUND,
        "el exceso termina la subida: se sube de nuevo desde cero"
    );

    Ok(())
}

#[tokio::test]
async fn cuerpo_mas_grande_que_el_tope_no_mata_la_subida(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let bytes = vec![0x55u8; 4];
    let blob_id = blob_de(&bytes);

    let creada = crear_subida(&ctx, 4, &blob_id).await?;
    let upload_id = upload_id_de(creada).await?;

    // Un cuerpo que ni siquiera entra en el tope declarado: puede ser un reconnect cortado.
    let enorme = parchear(&ctx, &upload_id, 0, vec![0x66u8; 5]).await?;
    assert_eq!(enorme.status(), StatusCode::BAD_REQUEST);

    // La subida sigue viva y reanudable desde 0.
    let viva = sonda(&ctx, &upload_id).await?;
    assert_eq!(viva.status(), StatusCode::OK);
    assert_eq!(header_i64(&viva, "upload-offset"), 0);

    let correcta = parchear(&ctx, &upload_id, 0, bytes).await?;
    assert_eq!(correcta.status(), StatusCode::NO_CONTENT);

    Ok(())
}

#[tokio::test]
async fn cuota_se_reserva_al_crear_y_bloquea_a_la_segunda(
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    cuota_del_vault(&ctx, 100).await?;

    let primera = crear_subida(&ctx, 60, &BlobId::new([1u8; 32])).await?;
    assert_eq!(primera.status(), StatusCode::CREATED);

    // La segunda ve la reserva de la primera: 60 + 50 > 100.
    let segunda = crear_subida(&ctx, 50, &BlobId::new([2u8; 32])).await?;
    assert_eq!(segunda.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let cuerpo = response_json(segunda).await?;
    assert_eq!(cuerpo["code"], "quota_exceeded");

    // Un PUT directo tampoco entra: ve la misma reserva.
    let bytes = contenido_de_prueba(50).await;
    let uri = format!(
        "/api/v1/vaults/{}/blobs/{}",
        ctx.vault_id,
        hex_lower(&Sha256::digest(&bytes))
    );
    let put = ctx
        .authenticated_request(Method::PUT, &uri, Body::from(bytes))
        .await?;
    assert_eq!(put.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // El margen exacto sí entra: 60 + 40 == 100.
    let al_limite = crear_subida(&ctx, 40, &BlobId::new([3u8; 32])).await?;
    assert_eq!(al_limite.status(), StatusCode::CREATED);

    Ok(())
}

#[tokio::test]
async fn la_reserva_se_libera_al_vencer_la_subida() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    cuota_del_vault(&ctx, 100).await?;

    let bloqueada = crear_subida(&ctx, 60, &BlobId::new([4u8; 32])).await?;
    let upload_id = upload_id_de(bloqueada).await?;

    // Un bloque real para que exista el temporal.
    let parcial = parchear(&ctx, &upload_id, 0, vec![0x77u8; 30]).await?;
    assert_eq!(parcial.status(), StatusCode::NO_CONTENT);

    let temporal = ctx
        .config
        .cas_root
        .join(".uploads")
        .join(format!("{upload_id}.part"));
    assert!(temporal.exists());

    let segunda = crear_subida(&ctx, 50, &BlobId::new([5u8; 32])).await?;
    assert_eq!(segunda.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // La subida se vence por reloj y la próxima creación barre: libera reserva Y temporal.
    sqlx::query("UPDATE uploads SET expires_at = datetime('now', '-1 minute') WHERE upload_id = ?")
        .bind(&upload_id)
        .execute(&ctx.pool)
        .await?;

    let ahora_si = crear_subida(&ctx, 50, &BlobId::new([5u8; 32])).await?;
    assert_eq!(ahora_si.status(), StatusCode::CREATED);
    assert!(!temporal.exists(), "el barrido se llevó el temporal vencido");

    // Y la vencida ya no existe para el protocolo.
    let muerta = sonda(&ctx, &upload_id).await?;
    assert_eq!(muerta.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn put_respeta_la_cuota() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    cuota_del_vault(&ctx, 100).await?;

    let primera = contenido_de_prueba(60).await;
    let uri_primera = format!(
        "/api/v1/vaults/{}/blobs/{}",
        ctx.vault_id,
        hex_lower(&Sha256::digest(&primera))
    );

    let put = ctx
        .authenticated_request(Method::PUT, &uri_primera, Body::from(primera.clone()))
        .await?;
    assert_eq!(put.status(), StatusCode::CREATED);

    let segunda = contenido_de_prueba(61).await;
    let uri_segunda = format!(
        "/api/v1/vaults/{}/blobs/{}",
        ctx.vault_id,
        hex_lower(&Sha256::digest(&segunda))
    );
    let put = ctx
        .authenticated_request(Method::PUT, &uri_segunda, Body::from(segunda))
        .await?;
    assert_eq!(put.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // Re-subir lo MISMO no consume espacio nuevo: entra aunque la cuota esté al tope.
    let put = ctx
        .authenticated_request(Method::PUT, &uri_primera, Body::from(primera))
        .await?;
    assert_eq!(put.status(), StatusCode::OK);

    Ok(())
}

#[tokio::test]
async fn upload_de_otro_vault_da_404() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;

    let creada = crear_subida(&ctx, 10, &BlobId::new([6u8; 32])).await?;
    let upload_id = upload_id_de(creada).await?;

    // Mismo dueño, otro vault suyo: 404 (no se confirma existencia).
    let vault_b = ctx.agregar_vault_propio("vault-b").await?;
    let request = Request::builder()
        .method(Method::HEAD)
        .uri(format!("/api/v1/vaults/{vault_b}/uploads/{upload_id}"))
        .header(header::AUTHORIZATION, format!("Bearer {}", ctx.token))
        .header("tus-resumable", TUS)
        .body(Body::empty())?;
    let ajeno = ctx.request(request).await?;
    assert_eq!(ajeno.status(), StatusCode::NOT_FOUND);

    // Otra cuenta: tampoco.
    let (vault_c, token_c) = ctx.agregar_vault_ajeno("ajeno@example.com").await?;
    let request = Request::builder()
        .method(Method::HEAD)
        .uri(format!("/api/v1/vaults/{vault_c}/uploads/{upload_id}"))
        .header(header::AUTHORIZATION, format!("Bearer {token_c}"))
        .header("tus-resumable", TUS)
        .body(Body::empty())?;
    let ajeno2 = ctx.request(request).await?;
    assert_eq!(ajeno2.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn crear_exige_tus_length_y_metadata() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;
    let blob_id = BlobId::new([8u8; 32]);
    let metadata = format!("blob_id {}", STANDARD.encode(blob_id.as_bytes()));
    let base = format!("/api/v1/vaults/{}/blobs", ctx.vault_id);

    // Sin Tus-Resumable.
    let request = Request::builder()
        .method(Method::POST)
        .uri(&base)
        .header(header::AUTHORIZATION, format!("Bearer {}", ctx.token))
        .header("upload-length", "10")
        .header("upload-metadata", &metadata)
        .body(Body::empty())?;
    assert_eq!(
        ctx.request(request).await?.status(),
        StatusCode::BAD_REQUEST
    );

    // Sin Upload-Metadata.
    let request = Request::builder()
        .method(Method::POST)
        .uri(&base)
        .header(header::AUTHORIZATION, format!("Bearer {}", ctx.token))
        .header("tus-resumable", TUS)
        .header("upload-length", "10")
        .body(Body::empty())?;
    assert_eq!(
        ctx.request(request).await?.status(),
        StatusCode::BAD_REQUEST
    );

    // Upload-Length cero.
    let request = Request::builder()
        .method(Method::POST)
        .uri(&base)
        .header(header::AUTHORIZATION, format!("Bearer {}", ctx.token))
        .header("tus-resumable", TUS)
        .header("upload-length", "0")
        .header("upload-metadata", &metadata)
        .body(Body::empty())?;
    assert_eq!(
        ctx.request(request).await?.status(),
        StatusCode::BAD_REQUEST
    );

    // El techo del cuerpo es CONTENIDO + sobre (nonce‖tag, ADR-0014): el máximo exacto entra.
    let tope = ovaultd_protocol::MAX_FILE_BYTES + ovaultd_crypto::BLOB_ENVELOPE_BYTES;

    let al_limite = crear_subida(&ctx, tope, &blob_id).await?;
    assert_eq!(al_limite.status(), StatusCode::CREATED);

    let excedido = crear_subida(&ctx, tope + 1, &blob_id).await?;
    assert_eq!(excedido.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let cuerpo = response_json(excedido).await?;
    assert_eq!(cuerpo["code"], "max_file_exceeded");

    Ok(())
}

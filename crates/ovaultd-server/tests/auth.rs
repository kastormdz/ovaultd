//! Integración de `auth.rs`: challenges (uso único, vencimiento, ligados a device y vault) y
//! revocación de devices contra la base real y el router completo.

mod common;

use axum::{
    body::Body,
    http::{header, Method, Request, StatusCode},
};
use common::{hex_lower, response_json, TestContext, EMAIL, PASSWORD};
use ovaultd_server::{auth, ServerError};
use sha2::{Digest, Sha256};

#[tokio::test]
async fn challenge_replay_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;

    let challenge = auth::emitir_challenge(&ctx.pool, ctx.device_id, ctx.vault_id).await?;

    auth::consumir_challenge(&ctx.pool, ctx.device_id, ctx.vault_id, &challenge).await?;

    let replay = auth::consumir_challenge(&ctx.pool, ctx.device_id, ctx.vault_id, &challenge).await;
    match replay {
        Err(ServerError::Unauthorized) => {}
        otro => panic!("el replay debía dar Unauthorized, vino {otro:?}"),
    }

    assert!(
        auth::challenge_pendiente(&ctx.pool, ctx.device_id, ctx.vault_id)
            .await?
            .is_none(),
        "un challenge consumido no queda pendiente"
    );

    Ok(())
}

#[tokio::test]
async fn challenge_vencido_no_se_puede_consumir() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;

    let challenge = auth::emitir_challenge(&ctx.pool, ctx.device_id, ctx.vault_id).await?;

    // Vencer por reloj, determinista: sin sleeps.
    sqlx::query(
        "UPDATE auth_challenges
         SET expires_at = datetime('now', '-1 minute')
         WHERE challenge = ?",
    )
    .bind(&challenge)
    .execute(&ctx.pool)
    .await?;

    assert!(
        auth::challenge_pendiente(&ctx.pool, ctx.device_id, ctx.vault_id)
            .await?
            .is_none(),
        "un challenge vencido no está pendiente"
    );

    assert!(
        auth::consumir_challenge(&ctx.pool, ctx.device_id, ctx.vault_id, &challenge)
            .await
            .is_err(),
        "un challenge vencido no se consume"
    );

    Ok(())
}

#[tokio::test]
async fn emitir_challenge_barre_los_vencidos() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;

    // Tres challenges vencidos (uno usado, dos nunca usados): todos son basura.
    for i in 0..3u8 {
        sqlx::query(
            "INSERT INTO auth_challenges (device_id, vault_id, challenge, expires_at, used_at)
             VALUES (?, ?, ?, datetime('now', '-1 hour'), ?)",
        )
        .bind(ctx.device_id)
        .bind(ctx.vault_id)
        .bind(vec![i; 32])
        .bind(if i == 0 {
            Some("2026-01-01 00:00:00")
        } else {
            None
        })
        .execute(&ctx.pool)
        .await?;
    }

    let vencidos_antes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM auth_challenges WHERE expires_at < datetime('now')",
    )
    .fetch_one(&ctx.pool)
    .await?;
    assert_eq!(vencidos_antes, 3);

    // Emitir barre los vencidos (perezoso, el patrón de `uploads::barrer` con el índice
    // `idx_challenges_expira`) y deja el recién emitido.
    let fresco = auth::emitir_challenge(&ctx.pool, ctx.device_id, ctx.vault_id).await?;

    let vencidos_despues: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM auth_challenges WHERE expires_at < datetime('now')",
    )
    .fetch_one(&ctx.pool)
    .await?;
    assert_eq!(vencidos_despues, 0, "el barrido deja la tabla sin vencidos");

    assert_eq!(
        auth::challenge_pendiente(&ctx.pool, ctx.device_id, ctx.vault_id)
            .await?
            .as_deref(),
        Some(fresco.as_slice()),
        "el challenge recién emitido no se barre"
    );

    Ok(())
}

#[tokio::test]
async fn challenge_ligado_al_device_y_al_vault() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;

    let challenge = auth::emitir_challenge(&ctx.pool, ctx.device_id, ctx.vault_id).await?;

    // Otro device de la MISMA cuenta no puede consumir el challenge ajeno.
    let (_, otro_device) =
        auth::iniciar_sesion(&ctx.pool, EMAIL, PASSWORD, "otro-device", None).await?;
    assert!(
        auth::consumir_challenge(&ctx.pool, otro_device, ctx.vault_id, &challenge)
            .await
            .is_err(),
        "el challenge está ligado al device que lo pidió"
    );

    // Y el device correcto, contra OTRO vault, tampoco.
    assert!(
        auth::consumir_challenge(&ctx.pool, ctx.device_id, ctx.vault_id + 1_000, &challenge)
            .await
            .is_err(),
        "el challenge está ligado al vault para el que se emitió"
    );

    // El dueño legítimo sigue pudiendo usarlo: los intentos inválidos no lo quemaron.
    auth::consumir_challenge(&ctx.pool, ctx.device_id, ctx.vault_id, &challenge).await?;

    Ok(())
}

#[tokio::test]
async fn revoked_device_cannot_download() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;

    // Con el token vigente, sube un blob legítimo.
    let bytes = b"contenido que no se debe poder bajar tras revocar";
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let uri = format!(
        "/api/v1/vaults/{}/blobs/{}",
        ctx.vault_id,
        hex_lower(&digest)
    );

    let put = ctx
        .authenticated_request(Method::PUT, &uri, Body::from(bytes.to_vec()))
        .await?;
    assert_eq!(put.status(), StatusCode::CREATED);

    // Revocar el device: el token deja de verificar.
    auth::revocar_device(&ctx.pool, ctx.device_id).await?;
    assert!(
        auth::verificar_token(&ctx.pool, &ctx.token).await.is_err(),
        "un token revocado no verifica"
    );

    // Y la descarga rebota con 401 (el device ya no es nadie).
    let get = ctx.authenticated_request(Method::GET, &uri, Body::empty()).await?;
    assert_eq!(get.status(), StatusCode::UNAUTHORIZED);

    let cuerpo = response_json(get).await?;
    assert_eq!(cuerpo["t"], "error");
    assert_eq!(cuerpo["code"], "unauthorized");

    Ok(())
}

#[tokio::test]
async fn token_basura_no_verifica() -> Result<(), Box<dyn std::error::Error>> {
    let ctx = TestContext::new().await?;

    assert!(auth::verificar_token(&ctx.pool, "no-es-un-token").await.is_err());

    let response = ctx
        .request_con_token("no-es-un-token", Method::GET, "/api/v1/me", Body::empty())
        .await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn el_signin_persiste_el_nombre_y_la_plataforma_del_cliente() -> Result<(), Box<dyn std::error::Error>> {
    let contexto = TestContext::new().await?;

    // El name/platform del signin es lo que el panel muestra para distinguir máquinas
    // (el plugin manda el hostname en escritorio o el modelo del equipo en Android): el
    // server tiene que persistir EXACTAMENTE lo que le manda el cliente.
    let respuesta = contexto
        .request(
            Request::post("/api/v1/auth/signin")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "email": EMAIL,
                        "password": PASSWORD,
                        "name": "SM-S918B",
                        "platform": "android",
                    })
                    .to_string(),
                ))?,
        )
        .await?;
    assert_eq!(respuesta.status(), StatusCode::OK);
    let cuerpo = response_json(respuesta).await?;
    let device_id = cuerpo["device_id"].as_i64().expect("device_id en la respuesta");

    let (nombre, plataforma): (String, Option<String>) =
        sqlx::query_as("SELECT name, platform FROM devices WHERE id = ?")
            .bind(device_id)
            .fetch_one(&contexto.pool)
            .await?;
    assert_eq!(nombre, "SM-S918B", "el nombre viaja tal cual");
    assert_eq!(plataforma.as_deref(), Some("android"), "la plataforma viaja tal cual");

    Ok(())
}

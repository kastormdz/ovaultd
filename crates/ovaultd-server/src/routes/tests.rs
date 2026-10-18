use std::sync::Arc;

use axum::{
    body::Body,
    http::{
        header::{AUTHORIZATION, CONTENT_RANGE, HOST, RANGE},
        HeaderValue, Request, StatusCode,
    },
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

use super::{router, EstadoServidor};
use crate::{
    auth,
    config::Config,
    storage::CasStore,
};

struct TestContext {
    pool: SqlitePool,
    _db_dir: TempDir,
    _cas_dir: TempDir,
    config: Config,
    cas: Arc<CasStore>,
    device_id: i64,
    token: String,
    vault_id: i64,
    keys: ovaultd_crypto::SubKeys,
}

impl TestContext {
    async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let db_dir = tempfile::tempdir()?;
        let cas_dir = tempfile::tempdir()?;

        let db_path = db_dir.path().join("metadata.db");
        let pool = crate::db::connect(db_path.to_str().ok_or("ruta inválida")?).await?;
        sqlx::migrate!("../../migrations").run(&pool).await?;

        let account =
            auth::crear_cuenta(&pool, "a@example.com", auth::PASSWORD_DE_PRUEBA).await?;
        let (token, device_id) = auth::iniciar_sesion(
            &pool,
            "a@example.com",
            auth::PASSWORD_DE_PRUEBA,
            "test-device",
            Some("test"),
        )
        .await?;

        let vault_key = ovaultd_crypto::VaultKey([7u8; 32]);
        let keys = ovaultd_crypto::derive_subkeys(&vault_key, &[8u8; 16]);
        let identity = ovaultd_crypto::VaultIdentity::generate(&keys.sign);
        let vault_pubkey = identity.pubkey();

        let vault_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO vaults
                 (account_id, name, salt, kdf_params, wrapped_key_pw,
                  wrapped_key_rc, vault_pubkey)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(account.id)
        .bind("test-vault")
        .bind(vec![8u8; 16])
        .bind("{}")
        .bind(vec![0u8; 32])
        .bind(vec![0u8; 32])
        .bind(vault_pubkey.to_vec())
        .fetch_one(&pool)
        .await?;

        let config = Config {
            bind_addr: "127.0.0.1:8080".to_owned(),
            database_path: db_path.to_string_lossy().into_owned(),
            cas_root: cas_dir.path().to_path_buf(),
            token_ttl_hours: 720,
            challenge_ttl_seconds: 300,
            upload_ttl_hours: 24,
            setup_token: None,
            secure_cookies: crate::config::ModoSecureCookies::Auto,
            trust_proxy: true,
        };

        let cas = Arc::new(CasStore::new(cas_dir.path().to_path_buf())?);

        Ok(Self {
            pool,
            _db_dir: db_dir,
            _cas_dir: cas_dir,
            config,
            cas,
            device_id,
            token,
            vault_id,
            keys,
        })
    }

    fn estado(&self) -> EstadoServidor {
        EstadoServidor {
            pool: self.pool.clone(),
            config: self.config.clone(),
            cas: self.cas.clone(),
            limitador_login: Default::default(),
            notifier: std::sync::Arc::new(crate::storage::Notifier::new()),
        }
    }

    fn router(&self) -> axum::Router {
        router(self.estado())
    }

    async fn request(
        &self,
        request: Request<Body>,
    ) -> Result<axum::response::Response, Box<dyn std::error::Error>> {
        Ok(self.router().oneshot(request).await?)
    }

    async fn authenticated_request(
        &self,
        method: axum::http::Method,
        uri: &str,
        body: Body,
    ) -> Result<axum::response::Response, Box<dyn std::error::Error>> {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", self.token))?,
            )
            .body(body)?;

        self.request(request).await
    }

    async fn insert_vault_for(
        &self,
        account_id: i64,
        name: &str,
    ) -> Result<i64, Box<dyn std::error::Error>> {
        let vault_key = ovaultd_crypto::VaultKey([9u8; 32]);
        let keys = ovaultd_crypto::derive_subkeys(&vault_key, &[10u8; 16]);
        let identity = ovaultd_crypto::VaultIdentity::generate(&keys.sign);

        Ok(sqlx::query_scalar::<_, i64>(
            "INSERT INTO vaults
                 (account_id, name, salt, kdf_params, wrapped_key_pw,
                  wrapped_key_rc, vault_pubkey)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(account_id)
        .bind(name)
        .bind(vec![10u8; 16])
        .bind("{}")
        .bind(vec![0u8; 32])
        .bind(vec![0u8; 32])
        .bind(identity.pubkey().to_vec())
        .fetch_one(&self.pool)
        .await?)
    }
}

async fn response_json(
    response: axum::response::Response,
) -> Result<Value, Box<dyn std::error::Error>> {
    let bytes = response.into_body().collect().await?.to_bytes();
    Ok(serde_json::from_slice(&bytes)?)
}

async fn response_bytes(
    response: axum::response::Response,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(response.into_body().collect().await?.to_bytes().to_vec())
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn signin_body(email: &str, password: &str) -> Body {
    Body::from(
        json!({
            "email": email,
            "password": password
        })
        .to_string(),
    )
}

fn vault_body(name: &str) -> Body {
    Body::from(
        json!({
            "name": name,
            "salt": STANDARD.encode([1u8; 16]),
            "kdf_params": {},
            "wrapped_key_pw": STANDARD.encode([2u8; 32]),
            "wrapped_key_rc": STANDARD.encode([3u8; 32]),
            "vault_pubkey": STANDARD.encode([4u8; 32])
        })
        .to_string(),
    )
}

/// Igual que `vault_body`, con el UUID de 16 B del cliente (ADR-0013) y parámetros de KDF.
fn vault_body_con_uuid(name: &str, uuid: [u8; 16]) -> Body {
    Body::from(
        json!({
            "name": name,
            "salt": STANDARD.encode([1u8; 16]),
            "kdf_params": { "m": 65536, "t": 3, "p": 4, "dkLen": 32 },
            "wrapped_key_pw": STANDARD.encode([2u8; 32]),
            "wrapped_key_rc": STANDARD.encode([3u8; 32]),
            "vault_pubkey": STANDARD.encode([4u8; 32]),
            "vault_id": STANDARD.encode(uuid),
        })
        .to_string(),
    )
}

fn vault_request(token: &str, body: Body) -> Result<Request<Body>, Box<dyn std::error::Error>> {
    Ok(Request::post("/api/v1/vaults")
        .header("content-type", "application/json")
        .header(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}"))?)
        .body(body)?)
}

#[tokio::test]
async fn signin_con_password_correcta_devuelve_token(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let response = context
        .request(
            Request::post("/api/v1/auth/signin")
                .header("content-type", "application/json")
                .body(signin_body("a@example.com", auth::PASSWORD_DE_PRUEBA))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await?;
    assert!(body["token"].as_str().is_some());
    assert!(body["device_id"].as_i64().is_some());

    Ok(())
}

#[tokio::test]
async fn signin_con_password_incorrecta_da_401(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let response = context
        .request(
            Request::post("/api/v1/auth/signin")
                .header("content-type", "application/json")
                .body(signin_body("a@example.com", "password-incorrecta"))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn signin_con_mfa_da_invalid_y_no_emite_token(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let response = context
        .request(
            Request::post("/api/v1/auth/signin")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "email": "a@example.com",
                        "password": auth::PASSWORD_DE_PRUEBA,
                        "mfa": "123456"
                    })
                    .to_string(),
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response_json(response).await?;
    assert!(body.get("token").is_none());

    Ok(())
}

#[tokio::test]
async fn healthz_responde_ok() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let response = context
        .request(
            Request::builder()
                .method(axum::http::Method::GET)
                .uri("/healthz")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await?;
    assert_eq!(body["status"], "ok");

    Ok(())
}

#[tokio::test]
async fn metrics_expone_solo_operacion() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let response = context
        .request(
            Request::builder()
                .method(axum::http::Method::GET)
                .uri("/metrics")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|valor| valor.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("text/plain"));

    let body = String::from_utf8(response_bytes(response).await?)?;
    assert!(body.contains("ovaultd_up 1"));
    assert!(body.contains("ovaultd_uptime_seconds"));
    assert!(!body.contains("token"), "metrics no expone secretos");

    Ok(())
}

#[tokio::test]
async fn sin_token_da_401() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let response = context
        .request(Request::get("/api/v1/me").body(Body::empty())?)
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn token_invalido_da_401() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let response = context
        .request(
            Request::get("/api/v1/me")
                .header(AUTHORIZATION, "Bearer token-invalido")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn token_revocado_da_401() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let signout = context
        .authenticated_request(
            axum::http::Method::POST,
            "/api/v1/auth/signout",
            Body::empty(),
        )
        .await?;

    assert_eq!(signout.status(), StatusCode::NO_CONTENT);

    let response = context
        .authenticated_request(axum::http::Method::GET, "/api/v1/me", Body::empty())
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn vault_de_otra_cuenta_da_404_no_403(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let other = auth::crear_cuenta(
        &context.pool,
        "b@example.com",
        auth::PASSWORD_DE_PRUEBA,
    )
    .await?;
    let other_vault = context
        .insert_vault_for(other.id, "vault-de-otra-cuenta")
        .await?;

    let response = context
        .authenticated_request(
            axum::http::Method::GET,
            &format!("/api/v1/vaults/{other_vault}/challenge"),
            Body::empty(),
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_ne!(response.status(), StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn crear_vault_devuelve_id_y_aparece_en_la_lista(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let request = Request::post("/api/v1/vaults")
        .header("content-type", "application/json")
        .header(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", context.token))?,
        )
        .body(vault_body("vault-creado"))?;

    let create = context.request(request).await?;

    assert_eq!(create.status(), StatusCode::CREATED);
    let created = response_json(create).await?;
    let vault_id = created["id"].as_i64().ok_or("falta id")?;

    let list = context
        .authenticated_request(
            axum::http::Method::GET,
            "/api/v1/vaults",
            Body::empty(),
        )
        .await?;

    assert_eq!(list.status(), StatusCode::OK);
    let vaults = response_json(list).await?;
    assert!(vaults
        .as_array()
        .ok_or("la lista no es un array")?
        .iter()
        .any(|vault| vault["id"] == vault_id));

    Ok(())
}

#[tokio::test]
async fn vault_id_repetido_da_409() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let uuid = [0x42u8; 16];

    let primera = context
        .request(vault_request(&context.token, vault_body_con_uuid("vault-con-uuid", uuid))?)
        .await?;
    assert_eq!(primera.status(), StatusCode::CREATED);

    // Mismo UUID otra vez (aunque cambie el nombre): 409 y no se insertó nada.
    let segunda = context
        .request(vault_request(&context.token, vault_body_con_uuid("vault-duplicado", uuid))?)
        .await?;
    assert_eq!(segunda.status(), StatusCode::CONFLICT);
    let cuerpo = response_json(segunda).await?;
    assert_eq!(cuerpo["code"], "bad_request");

    let filas: i64 = sqlx::query_scalar("SELECT count(*) FROM vaults WHERE vault_uuid IS NOT NULL")
        .fetch_one(&context.pool)
        .await?;
    assert_eq!(filas, 1, "el duplicado no inserta");

    Ok(())
}

#[tokio::test]
async fn material_devuelve_lo_publico_y_404_para_ajenos(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let uuid = [0x66u8; 16];

    let creada = context
        .request(vault_request(&context.token, vault_body_con_uuid("vault-material", uuid))?)
        .await?;
    assert_eq!(creada.status(), StatusCode::CREATED);
    let vault_id = response_json(creada).await?["id"]
        .as_i64()
        .ok_or("falta id")?;

    let material = context
        .authenticated_request(
            axum::http::Method::GET,
            &format!("/api/v1/vaults/{vault_id}/material"),
            Body::empty(),
        )
        .await?;
    assert_eq!(material.status(), StatusCode::OK);
    let cuerpo = response_json(material).await?;

    assert_eq!(cuerpo["vault_id"], STANDARD.encode(uuid));
    assert_eq!(cuerpo["salt"], STANDARD.encode([1u8; 16]));
    assert_eq!(cuerpo["vault_pubkey"], STANDARD.encode([4u8; 32]));
    assert_eq!(cuerpo["wrapped_key_pw"], STANDARD.encode([2u8; 32]));
    assert_eq!(cuerpo["enc_version"], 2);
    assert_eq!(cuerpo["epoch"], 1);

    // `kdf_params` vuelve como TEXTO tal cual se guardó: el cliente lo parsea.
    let params: Value = serde_json::from_str(
        cuerpo["kdf_params"]
            .as_str()
            .ok_or("kdf_params no es texto")?,
    )?;
    assert_eq!(params["m"], 65536);
    assert_eq!(params["dkLen"], 32);

    // Un vault de OTRA cuenta: 404 (nunca se confirma existencia ni material ajeno).
    let other = auth::crear_cuenta(
        &context.pool,
        "material-ajeno@example.com",
        auth::PASSWORD_DE_PRUEBA,
    )
    .await?;
    let other_vault = context
        .insert_vault_for(other.id, "vault-ajeno-material")
        .await?;
    let ajeno = context
        .authenticated_request(
            axum::http::Method::GET,
            &format!("/api/v1/vaults/{other_vault}/material"),
            Body::empty(),
        )
        .await?;
    assert_eq!(ajeno.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn blob_put_y_get_devuelve_los_mismos_bytes(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let bytes = b"contenido de prueba";
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let hash = hex_lower(&digest);

    let put = context
        .authenticated_request(
            axum::http::Method::PUT,
            &format!("/api/v1/vaults/{}/blobs/{hash}", context.vault_id),
            Body::from(bytes.to_vec()),
        )
        .await?;

    assert_eq!(put.status(), StatusCode::CREATED);

    let get = context
        .authenticated_request(
            axum::http::Method::GET,
            &format!("/api/v1/vaults/{}/blobs/{hash}", context.vault_id),
            Body::empty(),
        )
        .await?;

    assert_eq!(get.status(), StatusCode::OK);
    assert_eq!(response_bytes(get).await?, bytes);

    // Y el PUT deja el blob registrado en el indice: `commit_batch` rechazaria (`not_found`) la
    // referencia a un blob sin fila en `blobs`.
    let registrados: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM blobs WHERE vault_id = ? AND hash = ?",
    )
    .bind(context.vault_id)
    .bind(digest.as_slice())
    .fetch_one(&context.pool)
    .await?;
    assert_eq!(
        registrados, 1,
        "el PUT tiene que registrar el blob en el indice (blobs)"
    );

    Ok(())
}

#[tokio::test]
async fn blob_con_hash_que_no_coincide_da_400_y_no_guarda_nada(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let bytes = b"contenido cuyo hash no coincide";
    let requested_hash = [0u8; 32];
    let hash = hex_lower(&requested_hash);

    let response = context
        .authenticated_request(
            axum::http::Method::PUT,
            &format!("/api/v1/vaults/{}/blobs/{hash}", context.vault_id),
            Body::from(bytes.to_vec()),
        )
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!context
        .cas
        .exists(&ovaultd_protocol::BlobId::new(requested_hash)));

    // Ni fila en el indice: un hash que no coincide no deja NADA.
    let registrados: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM blobs WHERE vault_id = ? AND hash = ?",
    )
    .bind(context.vault_id)
    .bind(requested_hash.as_slice())
    .fetch_one(&context.pool)
    .await?;
    assert_eq!(registrados, 0, "un hash que no coincide no deja fila en blobs");

    Ok(())
}

#[tokio::test]
async fn blob_put_repetido_es_idempotente(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let bytes = b"blob idempotente";
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let hash = hex_lower(&digest);
    let uri = format!("/api/v1/vaults/{}/blobs/{hash}", context.vault_id);

    let first = context
        .authenticated_request(
            axum::http::Method::PUT,
            &uri,
            Body::from(bytes.to_vec()),
        )
        .await?;
    assert_eq!(first.status(), StatusCode::CREATED);

    let second = context
        .authenticated_request(
            axum::http::Method::PUT,
            &uri,
            Body::from(bytes.to_vec()),
        )
        .await?;
    assert_eq!(second.status(), StatusCode::OK);

    Ok(())
}

#[tokio::test]
async fn blob_get_con_range_devuelve_206_y_el_trozo(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let bytes = b"0123456789";
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let hash = hex_lower(&digest);
    let uri = format!("/api/v1/vaults/{}/blobs/{hash}", context.vault_id);

    let put = context
        .authenticated_request(
            axum::http::Method::PUT,
            &uri,
            Body::from(bytes.to_vec()),
        )
        .await?;
    assert_eq!(put.status(), StatusCode::CREATED);

    let request = Request::get(&uri)
        .header(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", context.token))?,
        )
        .header(RANGE, "bytes=2-5")
        .body(Body::empty())?;

    let response = context.request(request).await?;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.headers().get(CONTENT_RANGE).unwrap(),
        "bytes 2-5/10"
    );
    assert_eq!(response_bytes(response).await?, b"2345");

    Ok(())
}

#[tokio::test]
async fn changes_delega_en_delta_y_devuelve_los_items(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    sqlx::query(
        "INSERT INTO revisions
             (vault_id, rev, path_id, prev_path_id, op, path_enc,
              blob_hash, t_mac, size, mtime, expected_rev,
              operation_id, epoch, sig)
         VALUES (?, ?, ?, NULL, ?, ?, NULL, NULL, ?, ?, ?, ?, ?, ?)",
    )
    .bind(context.vault_id)
    .bind(1_i64)
    .bind(vec![1u8; 32])
    .bind("del")
    .bind(vec![2u8; 16])
    .bind(0_i64)
    .bind(0_i64)
    .bind(0_i64)
    // operation_id en la forma REAL del wire (UUID hex con guiones): NO base64.
    .bind("01010101-0101-0101-0101-010101010101")
    .bind(0_i64)
    .bind(vec![3u8; 64])
    .execute(&context.pool)
    .await?;

    sqlx::query("UPDATE vaults SET head_rev = 1 WHERE id = ?")
        .bind(context.vault_id)
        .execute(&context.pool)
        .await?;

    let response = context
        .authenticated_request(
            axum::http::Method::GET,
            &format!(
                "/api/v1/vaults/{}/changes?since=0&limit=100",
                context.vault_id
            ),
            Body::empty(),
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await?;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["next_since"], 1);

    Ok(())
}

#[tokio::test]
async fn access_con_firma_valida_devuelve_host_y_rev(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let challenge_response = context
        .authenticated_request(
            axum::http::Method::GET,
            &format!(
                "/api/v1/vaults/{}/challenge",
                context.vault_id
            ),
            Body::empty(),
        )
        .await?;
    assert_eq!(challenge_response.status(), StatusCode::OK);

    let challenge_body = response_json(challenge_response).await?;
    let challenge = STANDARD.decode(
        challenge_body["challenge"]
            .as_str()
            .ok_or("falta challenge")?,
    )?;

    let proof = ovaultd_crypto::sign_proof(
        &context.keys,
        context.vault_id,
        context.device_id,
        &challenge,
    );

    let request = Request::post(format!(
        "/api/v1/vaults/{}/access",
        context.vault_id
    ))
    .header(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", context.token))?,
    )
    .header(HOST, "sync.example.test")
    .header("content-type", "application/json")
    .body(Body::from(
        json!({ "proof": STANDARD.encode(proof) }).to_string(),
    ))?;

    let response = context.request(request).await?;
    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await?;
    assert_eq!(body["host"], "sync.example.test");
    assert_eq!(body["rev"], 0);

    Ok(())
}

#[tokio::test]
async fn access_con_firma_invalida_da_401_y_no_quema_el_challenge(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let challenge_response = context
        .authenticated_request(
            axum::http::Method::GET,
            &format!(
                "/api/v1/vaults/{}/challenge",
                context.vault_id
            ),
            Body::empty(),
        )
        .await?;
    assert_eq!(challenge_response.status(), StatusCode::OK);

    let challenge_body = response_json(challenge_response).await?;
    let challenge = STANDARD.decode(
        challenge_body["challenge"]
            .as_str()
            .ok_or("falta challenge")?,
    )?;

    let invalid = Request::post(format!(
        "/api/v1/vaults/{}/access",
        context.vault_id
    ))
    .header(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", context.token))?,
    )
    .header("content-type", "application/json")
    .body(Body::from(
        json!({ "proof": STANDARD.encode([0u8; 64]) }).to_string(),
    ))?;

    let invalid_response = context.request(invalid).await?;
    assert_eq!(invalid_response.status(), StatusCode::UNAUTHORIZED);

    let valid_proof = ovaultd_crypto::sign_proof(
        &context.keys,
        context.vault_id,
        context.device_id,
        &challenge,
    );

    let valid = Request::post(format!(
        "/api/v1/vaults/{}/access",
        context.vault_id
    ))
    .header(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", context.token))?,
    )
    .header("content-type", "application/json")
    .body(Body::from(
        json!({ "proof": STANDARD.encode(valid_proof) }).to_string(),
    ))?;

    let valid_response = context.request(valid).await?;
    assert_eq!(valid_response.status(), StatusCode::OK);

    Ok(())
}

#[tokio::test]
async fn access_consume_el_challenge() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let challenge_response = context
        .authenticated_request(
            axum::http::Method::GET,
            &format!(
                "/api/v1/vaults/{}/challenge",
                context.vault_id
            ),
            Body::empty(),
        )
        .await?;
    let challenge_body = response_json(challenge_response).await?;
    let challenge = STANDARD.decode(
        challenge_body["challenge"]
            .as_str()
            .ok_or("falta challenge")?,
    )?;

    let proof = ovaultd_crypto::sign_proof(
        &context.keys,
        context.vault_id,
        context.device_id,
        &challenge,
    );
    let body = Body::from(
        json!({ "proof": STANDARD.encode(proof) }).to_string(),
    );

    let first = context
        .authenticated_request(
            axum::http::Method::POST,
            &format!("/api/v1/vaults/{}/access", context.vault_id),
            body,
        )
        .await?;
    assert_eq!(first.status(), StatusCode::OK);

    let second_proof = ovaultd_crypto::sign_proof(
        &context.keys,
        context.vault_id,
        context.device_id,
        &challenge,
    );

    let second = context
        .authenticated_request(
            axum::http::Method::POST,
            &format!("/api/v1/vaults/{}/access", context.vault_id),
            Body::from(
                json!({ "proof": STANDARD.encode(second_proof) }).to_string(),
            ),
        )
        .await?;
    assert_eq!(second.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

// Observaciones sobre routes.rs:
// - El contrato indica que un Range inválido o fuera de rango debe responder 416,
//   pero la implementación convierte esos casos en ServerError::Invalid.
// - El contrato describe `EstadoServidor` con `pool` y `config`; la implementación
//   actual también requiere `cas`, por lo que los tests construyen el estado con ese campo.
// - El contrato menciona que el hash de blobs inválido se procesa según la superficie
//   HTTP; la implementación rechaza hashes con letras mayúsculas mediante parse_blob_hash,
//   como exige la representación hexadecimal minúscula.
// - `salt`, `wrapped_key_pw`, `wrapped_key_rc` y `vault_pubkey`: el contrato indica
//   bytes en base64 en el wire; el handler acepta tanto strings base64 como arrays JSON
//   de números mediante `decode_bytes`.

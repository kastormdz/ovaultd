use std::sync::Arc;

use axum::{
    body::Body,
    extract::{
        FromRequestParts, Json, Path, Query, Request, State,
    },
    http::{
        header::{AUTHORIZATION, CONTENT_RANGE, CONTENT_TYPE, HOST, LOCATION, RANGE},
        request::Parts,
        HeaderMap, HeaderValue, StatusCode,
    },
    response::IntoResponse,
    routing::{delete, get, head, post, put},
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool};
use ovaultd_protocol::BlobId;

use crate::{
    auth,
    config::Config,
    delta,
    error::{Result, ServerError},
    storage::CasStore,
};

#[derive(Clone)]
pub struct EstadoServidor {
    pub pool: SqlitePool,
    pub config: Config,
    pub cas: Arc<CasStore>,
}

pub fn router(estado: EstadoServidor) -> Router {
    // Marca de arranque para `/metrics` (idempotente: el primer router montado manda).
    let _ = ARRANQUE.set(std::time::Instant::now());

    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .route("/api/v1/auth/signin", post(signin))
        .route("/api/v1/auth/signout", post(signout))
        .route("/api/v1/me", get(me))
        .route("/api/v1/vaults", get(listar_vaults).post(crear_vault))
        .route(
            "/api/v1/vaults/{id}/material",
            get(material_vault),
        )
        .route(
            "/api/v1/vaults/{id}/challenge",
            get(challenge),
        )
        .route(
            "/api/v1/vaults/{id}/access",
            post(access),
        )
        .route(
            "/api/v1/vaults/{id}/changes",
            get(changes),
        )
        .route(
            "/api/v1/vaults/{id}/blobs/{hash}",
            put(put_blob).get(get_blob),
        )
        .route(
            "/api/v1/vaults/{id}/blobs",
            post(crear_upload),
        )
        .route(
            "/api/v1/vaults/{id}/uploads/{upload_id}",
            head(estado_upload).patch(parchear_upload),
        )
        .route("/api/v1/devices", get(listar_devices).post(crear_device))
        .route("/api/v1/devices/{id}", delete(revocar_device))
        .route("/api/v1/ws", axum::routing::get(crate::ws::authenticated_handler))
        .with_state(estado)
}

/// Marca de arranque del proceso para `/metrics` (la fija el primer router montado).
static ARRANQUE: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// `GET /healthz` — liveness/readiness para el healthcheck del compose y los balanceadores.
///
/// Sin auth y sin datos de ningún vault: responde si la base contesta (`SELECT 1`). Lo usan el
/// `HEALTHCHECK` del contenedor y el `curl --fail .../healthz` de la aceptación de la fase.
async fn healthz(State(state): State<EstadoServidor>) -> impl IntoResponse {
    match sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ok" })),
        ),
        Err(error) => {
            tracing::error!(error = %error, "healthz: la base no responde");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "status": "degraded" })),
            )
        }
    }
}

/// `GET /metrics` — Prometheus mínimo y SOLO de operación: ni secretos ni payloads.
///
/// Si no querés que sea público, restringilo en el proxy (`docker/proxy/nginx.conf.example`).
async fn metrics() -> impl IntoResponse {
    let uptime = ARRANQUE
        .get()
        .map(|arranque| arranque.elapsed().as_secs())
        .unwrap_or(0);

    let cuerpo = format!(
        "# HELP ovaultd_up El proceso responde.\n\
         # TYPE ovaultd_up gauge\n\
         ovaultd_up 1\n\
         # HELP ovaultd_uptime_seconds Segundos desde el arranque del proceso.\n\
         # TYPE ovaultd_uptime_seconds counter\n\
         ovaultd_uptime_seconds {uptime}\n"
    );

    (
        StatusCode::OK,
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        cuerpo,
    )
}

pub struct Autenticado {
    pub account_id: i64,
    pub device_id: i64,
}

impl FromRequestParts<EstadoServidor> for Autenticado {
    type Rejection = ServerError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &EstadoServidor,
    ) -> std::result::Result<Self, Self::Rejection> {
        let value = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(ServerError::Unauthorized)?;

        let token = value
            .strip_prefix("Bearer ")
            .filter(|token| !token.is_empty())
            .ok_or(ServerError::Unauthorized)?;

        let device_id = auth::verificar_token(&state.pool, token)
            .await
            .map_err(|_| ServerError::Unauthorized)?;

        let account_id = sqlx::query_scalar::<_, i64>(
            "SELECT account_id
             FROM devices
             WHERE id = ?",
        )
        .bind(device_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ServerError::Unauthorized)?;

        Ok(Self {
            account_id,
            device_id,
        })
    }
}

#[derive(Debug, Deserialize)]
struct SigninRequest {
    email: String,
    password: String,
    #[serde(default)]
    mfa: Option<Value>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    platform: Option<String>,
}

#[derive(Debug, Serialize)]
struct SigninResponse {
    token: String,
    device_id: i64,
}

async fn signin(
    State(state): State<EstadoServidor>,
    Json(request): Json<SigninRequest>,
) -> Result<Json<SigninResponse>> {
    if request.mfa.is_some() {
        return Err(ServerError::Invalid(
            "mfa no implementado: no mandes el código, no se está verificando".to_owned(),
        ));
    }

    let (token, device_id) = auth::iniciar_sesion(
        &state.pool,
        &request.email,
        &request.password,
        request.name.as_deref().unwrap_or("default"),
        request.platform.as_deref(),
    )
    .await?;

    Ok(Json(SigninResponse { token, device_id }))
}

async fn signout(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
) -> Result<StatusCode> {
    auth::revocar_device(&state.pool, autenticado.device_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
struct DeviceResponse {
    id: i64,
    name: String,
    platform: Option<String>,
    created_at: String,
    revoked_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct MeResponse {
    account_id: i64,
    email: String,
    devices: Vec<DeviceResponse>,
}

#[derive(Debug, FromRow)]
struct DeviceRow {
    id: i64,
    name: String,
    platform: Option<String>,
    created_at: String,
    revoked_at: Option<String>,
}

async fn me(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
) -> Result<Json<MeResponse>> {
    let account = sqlx::query_as::<_, (i64, String)>(
        "SELECT id, email
         FROM accounts
         WHERE id = ?",
    )
    .bind(autenticado.account_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ServerError::Unauthorized)?;

    let devices = sqlx::query_as::<_, DeviceRow>(
        "SELECT id, name, platform, created_at, revoked_at
         FROM devices
         WHERE account_id = ?
         ORDER BY id",
    )
    .bind(autenticado.account_id)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|device| DeviceResponse {
        id: device.id,
        name: device.name,
        platform: device.platform,
        created_at: device.created_at,
        revoked_at: device.revoked_at,
    })
    .collect();

    Ok(Json(MeResponse {
        account_id: account.0,
        email: account.1,
        devices,
    }))
}

#[derive(Debug, Serialize)]
struct VaultResponse {
    id: i64,
    name: String,
    role: String,
    rev: i64,
}

#[derive(Debug, FromRow)]
struct VaultRow {
    id: i64,
    name: String,
    head_rev: i64,
}

async fn listar_vaults(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
) -> Result<Json<Vec<VaultResponse>>> {
    let vaults = sqlx::query_as::<_, VaultRow>(
        "SELECT id, name, head_rev
         FROM vaults
         WHERE account_id = ?
         ORDER BY id",
    )
    .bind(autenticado.account_id)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(
        vaults
            .into_iter()
            .map(|vault| VaultResponse {
                id: vault.id,
                name: vault.name,
                role: "owner".to_owned(),
                rev: vault.head_rev,
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize)]
struct CreateVaultRequest {
    name: String,
    salt: Value,
    kdf_params: Value,
    wrapped_key_pw: Value,
    #[serde(default)]
    wrapped_key_rc: Option<Value>,
    vault_pubkey: Value,
    /// UUID de 16 B del CLIENTE (ADR-0013): va en los AAD de las envueltas y en `msg_bind`.
    /// Opcional por retrocompatibilidad; cuando viene, se persiste y es único.
    #[serde(default)]
    vault_id: Option<Value>,
    #[serde(default)]
    max_file_bytes: Option<i64>,
}

fn decode_bytes(value: &Value, field: &str) -> Result<Vec<u8>> {
    match value {
        Value::String(value) => STANDARD
            .decode(value)
            .map_err(|error| ServerError::Invalid(format!("{field} inválido: {error}"))),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| u8::try_from(value).ok())
                    .ok_or_else(|| {
                        ServerError::Invalid(format!("{field} debe contener bytes válidos"))
                    })
            })
            .collect(),
        _ => Err(ServerError::Invalid(format!(
            "{field} debe ser base64 o un arreglo de bytes"
        ))),
    }
}

async fn crear_vault(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Json(request): Json<CreateVaultRequest>,
) -> Result<axum::response::Response> {
    let salt = decode_bytes(&request.salt, "salt")?;
    let wrapped_key_pw = decode_bytes(&request.wrapped_key_pw, "wrapped_key_pw")?;
    let wrapped_key_rc = request
        .wrapped_key_rc
        .as_ref()
        .map(|value| decode_bytes(value, "wrapped_key_rc"))
        .transpose()?
        .unwrap_or_default();
    let vault_pubkey = decode_bytes(&request.vault_pubkey, "vault_pubkey")?;

    let vault_uuid = request
        .vault_id
        .as_ref()
        .map(|value| decode_bytes(value, "vault_id"))
        .transpose()?;

    if let Some(uuid) = &vault_uuid {
        if uuid.len() != 16 {
            return Err(ServerError::Invalid(
                "vault_id debe tener 16 bytes (UUID binario)".to_owned(),
            ));
        }
    }

    // Atómico: el índice único parcial de `vault_uuid` hace que un duplicado no inserte nada
    // (ADR-0013). Filas sin `vault_id` (retrocompatibles) no participan del conflicto.
    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO vaults
            (account_id, name, salt, kdf_params,
             wrapped_key_pw, wrapped_key_rc, vault_pubkey, vault_uuid, max_file_bytes)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT DO NOTHING
         RETURNING id",
    )
    .bind(autenticado.account_id)
    .bind(request.name)
    .bind(salt)
    .bind(request.kdf_params.to_string())
    .bind(wrapped_key_pw)
    .bind(wrapped_key_rc)
    .bind(vault_pubkey)
    .bind(vault_uuid)
    .bind(request.max_file_bytes)
    .fetch_optional(&state.pool)
    .await?;

    let Some(id) = id else {
        // El único conflicto posible acá es `vault_uuid` duplicado (los demás son FK del
        // account autenticado). La lista de códigos es cerrada: el 409 va en el status.
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "t": "error",
                "code": "bad_request",
                "detail": "ese vault_id ya está registrado",
            })),
        )
            .into_response());
    };

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id })),
    )
        .into_response())
}

#[derive(Debug, FromRow)]
struct VaultMaterialRow {
    vault_uuid: Option<Vec<u8>>,
    name: String,
    enc_version: i64,
    epoch: i64,
    salt: Vec<u8>,
    kdf_alg: String,
    kdf_params: String,
    wrapped_key_pw: Vec<u8>,
    wrapped_key_rc: Vec<u8>,
    vault_pubkey: Vec<u8>,
}

/// `GET /api/v1/vaults/{id}/material` — el material **público** del vault (ADR-0013).
///
/// Es lo que un device nuevo necesita para derivar el KEK con la password, unwrappear la
/// `vault_key` y probar posesión. Nada de esto es secreto (las envueltas son ciphertext; la
/// password y la recovery jamás viajan al servidor). `kdf_params` va TAL CUAL se guardó
/// (texto JSON, byte-fiel): el server no lo re-serializa.
async fn material_vault(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path(vault_id): Path<i64>,
) -> Result<Json<Value>> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    let material = sqlx::query_as::<_, VaultMaterialRow>(
        "SELECT vault_uuid, name, enc_version, epoch, salt, kdf_alg, kdf_params,
                wrapped_key_pw, wrapped_key_rc, vault_pubkey
         FROM vaults
         WHERE id = ?",
    )
    .bind(vault_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ServerError::NotFound)?;

    Ok(Json(serde_json::json!({
        "id": vault_id,
        "vault_id": material.vault_uuid.as_deref().map(|uuid| STANDARD.encode(uuid)),
        "name": material.name,
        "enc_version": material.enc_version,
        "epoch": material.epoch,
        "salt": STANDARD.encode(&material.salt),
        "kdf_alg": material.kdf_alg,
        "kdf_params": material.kdf_params,
        "wrapped_key_pw": STANDARD.encode(&material.wrapped_key_pw),
        "wrapped_key_rc": STANDARD.encode(&material.wrapped_key_rc),
        "vault_pubkey": STANDARD.encode(&material.vault_pubkey),
    })))
}

pub(crate) async fn resolve_vault(
    pool: &SqlitePool,
    account_id: i64,
    vault_id: i64,
) -> Result<()> {
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT id
         FROM vaults
         WHERE id = ? AND account_id = ?",
    )
    .bind(vault_id)
    .bind(account_id)
    .fetch_optional(pool)
    .await?;

    if exists.is_none() {
        return Err(ServerError::NotFound);
    }

    Ok(())
}

async fn challenge(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path(vault_id): Path<i64>,
) -> Result<Json<Value>> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    let challenge =
        auth::emitir_challenge(&state.pool, autenticado.device_id, vault_id).await?;

    Ok(Json(serde_json::json!({
        "challenge": STANDARD.encode(challenge)
    })))
}

#[derive(Debug, Deserialize)]
struct AccessRequest {
    proof: String,
}

async fn access(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path(vault_id): Path<i64>,
    request: Request<Body>,
) -> Result<Json<Value>> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    let headers = request.headers().clone();
    let body = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|_| ServerError::Invalid("cuerpo inválido".to_owned()))?;

    let access: AccessRequest = serde_json::from_slice(&body)
        .map_err(|error| ServerError::Invalid(format!("cuerpo inválido: {error}")))?;

    let proof_bytes = STANDARD
        .decode(access.proof)
        .map_err(|_| ServerError::Unauthorized)?;

    let proof: [u8; 64] = proof_bytes
        .try_into()
        .map_err(|_| ServerError::Unauthorized)?;

    let challenge =
        auth::challenge_pendiente(&state.pool, autenticado.device_id, vault_id)
            .await?
            .ok_or(ServerError::Unauthorized)?;

    let public_key = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT vault_pubkey
         FROM vaults
         WHERE id = ? AND account_id = ?",
    )
    .bind(vault_id)
    .bind(autenticado.account_id)
    .fetch_one(&state.pool)
    .await?;

    let public_key: [u8; 32] = public_key
        .try_into()
        .map_err(|_| ServerError::Unauthorized)?;

    ovaultd_crypto::verify_proof(
        &public_key,
        vault_id,
        autenticado.device_id,
        &challenge,
        &proof,
    )
    .map_err(|_| ServerError::Unauthorized)?;

    auth::consumir_challenge(
        &state.pool,
        autenticado.device_id,
        vault_id,
        &challenge,
    )
    .await?;

    let rev = sqlx::query_scalar::<_, i64>(
        "SELECT head_rev
         FROM vaults
         WHERE id = ? AND account_id = ?",
    )
    .bind(vault_id)
    .bind(autenticado.account_id)
    .fetch_one(&state.pool)
    .await?;

    let host = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| state.config.bind_addr.clone());

    Ok(Json(serde_json::json!({
        "host": host,
        "rev": rev
    })))
}

#[derive(Debug, Deserialize)]
struct ChangesQuery {
    #[serde(default)]
    since: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

async fn changes(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path(vault_id): Path<i64>,
    Query(query): Query<ChangesQuery>,
) -> Result<Json<ovaultd_protocol::Changes>> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    let since = query.since.unwrap_or(0);
    let limit = query.limit.unwrap_or(100).clamp(0, 1000);

    Ok(Json(
        delta::changes(&state.pool, vault_id, since, limit).await?,
    ))
}

fn parse_blob_hash(value: &str) -> Result<BlobId> {
    if value.len() != 64
        || !value
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Err(ServerError::Invalid(
            "el hash debe ser hexadecimal minúsculo de 64 caracteres".to_owned(),
        ));
    }

    let mut bytes = [0u8; 32];

    for index in 0..32 {
        bytes[index] = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| ServerError::Invalid("hash inválido".to_owned()))?;
    }

    Ok(BlobId::new(bytes))
}

async fn blob_limit(pool: &SqlitePool, vault_id: i64) -> Result<i64> {
    let max_file_bytes = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT max_file_bytes
         FROM vaults
         WHERE id = ?",
    )
    .bind(vault_id)
    .fetch_one(pool)
    .await?;

    // El tope es de CONTENIDO (plaintext), nunca por encima del de protocolo. El cuerpo subido es
    // el blob almacenado, que agrega el sobre `nonce‖tag` (ADR-0014): se le suma acá, para que un
    // archivo del tamaño máximo siga entrando.
    let cap = i64::try_from(ovaultd_protocol::MAX_FILE_BYTES).unwrap_or(i64::MAX);
    let contenido = max_file_bytes.unwrap_or(cap).min(cap);
    let sobre = i64::try_from(ovaultd_crypto::BLOB_ENVELOPE_BYTES).unwrap_or(40);

    Ok(contenido + sobre)
}

async fn put_blob(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path((vault_id, hash)): Path<(i64, String)>,
    request: Request<Body>,
) -> Result<impl IntoResponse> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    let blob_id = parse_blob_hash(&hash)?;
    let max_bytes = blob_limit(&state.pool, vault_id).await?;

    let bytes = axum::body::to_bytes(request.into_body(), max_bytes as usize)
        .await
        .map_err(|_| ServerError::Protocol(ovaultd_protocol::ErrorCode::MaxFileExceeded))?;

    let digest = Sha256::digest(&bytes);
    if digest.as_slice() != blob_id.as_bytes() {
        return Err(ServerError::Invalid(
            "el hash del cuerpo no coincide con el hash solicitado".to_owned(),
        ));
    }

    let size = i64::try_from(bytes.len())
        .map_err(|_| ServerError::Invalid("blob demasiado grande".to_owned()))?;

    // Cuota opt-in (ADR-0012): sin espacio no se publica NI se registra nada.
    if !crate::storage::espacio_disponible(&state.pool, vault_id, &blob_id, size).await? {
        return Err(ServerError::Protocol(ovaultd_protocol::ErrorCode::QuotaExceeded));
    }

    if state.cas.exists(&blob_id) {
        // Idempotente, y además cura: si el CAS tiene los bytes pero falta la fila del índice
        // (crash entre publish y registro), re-subir el mismo blob la deja consistente.
        crate::storage::registrar_blob(&state.pool, vault_id, &blob_id, size).await?;
        return Ok(StatusCode::OK);
    }

    state.cas.publish(&blob_id, &bytes).await?;

    // El índice primero que la referencia: `commit_batch` rechaza (`not_found`) un batch que
    // apunte a un blob sin fila en `blobs`.
    crate::storage::registrar_blob(&state.pool, vault_id, &blob_id, size).await?;

    Ok(StatusCode::CREATED)
}

async fn get_blob(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path((vault_id, hash)): Path<(i64, String)>,
    headers: HeaderMap,
) -> Result<impl IntoResponse> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    let blob_id = parse_blob_hash(&hash)?;

    // El CAS es compartido entre TODOS los vaults (un solo root por hash): la frontera es el
    // índice. Sin esta verificación, conocer el hash alcanzaba para leer el blob de otro vault.
    if !crate::storage::blob_registrado(&state.pool, vault_id, &blob_id).await? {
        return Err(ServerError::NotFound);
    }

    let bytes = state.cas.read(&blob_id).await?;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );

    let Some(range) = headers.get(RANGE) else {
        return Ok((StatusCode::OK, response_headers, bytes));
    };

    let range = range
        .to_str()
        .map_err(|_| ServerError::Invalid("Range inválido".to_owned()))?;

    let range = range
        .strip_prefix("bytes=")
        .ok_or_else(|| ServerError::Invalid("Range inválido".to_owned()))?;

    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| ServerError::Invalid("Range inválido".to_owned()))?;

    let length = bytes.len() as u64;
    if length == 0 {
        return Err(ServerError::Invalid("Range fuera de rango".to_owned()));
    }

    let start = start
        .parse::<u64>()
        .map_err(|_| ServerError::Invalid("Range inválido".to_owned()))?;

    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>()
            .map_err(|_| ServerError::Invalid("Range inválido".to_owned()))?
    };

    if start > end || start >= length || end >= length {
        return Err(ServerError::Invalid("Range fuera de rango".to_owned()));
    }

    let content_range = format!("bytes {start}-{end}/{length}");
    response_headers.insert(
        CONTENT_RANGE,
        HeaderValue::from_str(&content_range)
            .map_err(|_| ServerError::Invalid("Content-Range inválido".to_owned()))?,
    );

    Ok((
        StatusCode::PARTIAL_CONTENT,
        response_headers,
        bytes[start as usize..=end as usize].to_vec(),
    ))
}

/// Nombres de los headers de tus core (no existen como constantes en `http`: tus no es HTTP puro).
const TUS_RESUMABLE: &str = "tus-resumable";
const UPLOAD_LENGTH: &str = "upload-length";
const UPLOAD_OFFSET: &str = "upload-offset";
const UPLOAD_METADATA: &str = "upload-metadata";
const OFFSET_OCTET_STREAM: &str = "application/offset+octet-stream";

fn exigir_tus(headers: &HeaderMap) -> Result<()> {
    match headers.get(TUS_RESUMABLE).and_then(|valor| valor.to_str().ok()) {
        Some("1.0.0") => Ok(()),
        _ => Err(ServerError::Invalid(
            "se requiere Tus-Resumable: 1.0.0".to_owned(),
        )),
    }
}

fn header_entero(headers: &HeaderMap, nombre: &str) -> Result<i64> {
    let valor = headers
        .get(nombre)
        .and_then(|valor| valor.to_str().ok())
        .ok_or_else(|| ServerError::Invalid(format!("falta el header {nombre}")))?;

    valor
        .parse::<i64>()
        .map_err(|_| ServerError::Invalid(format!("{nombre} inválido: {valor:?}")))
}

fn header_i64(valor: i64) -> HeaderValue {
    HeaderValue::from_str(&valor.to_string()).unwrap_or_else(|_| HeaderValue::from_static("0"))
}

/// `Upload-Metadata: blob_id <base64>` — la MISMA forma que el `blob_head` del WS (32 B).
fn parse_blob_metadata(headers: &HeaderMap) -> Result<BlobId> {
    let metadata = headers
        .get(UPLOAD_METADATA)
        .and_then(|valor| valor.to_str().ok())
        .ok_or_else(|| ServerError::Invalid("falta Upload-Metadata".to_owned()))?;

    for par in metadata.split(',') {
        let Some((clave, valor)) = par.trim().split_once(' ') else {
            continue;
        };

        if clave != "blob_id" {
            continue;
        }

        let bytes = STANDARD.decode(valor.trim()).map_err(|_| {
            ServerError::Invalid("el blob_id del metadata no es base64 válido".to_owned())
        })?;

        if bytes.len() != 32 {
            return Err(ServerError::Invalid(
                "el blob_id del metadata debe tener 32 bytes".to_owned(),
            ));
        }

        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes);
        return Ok(BlobId::new(hash));
    }

    Err(ServerError::Invalid(
        "Upload-Metadata sin blob_id".to_owned(),
    ))
}

/// `POST /api/v1/vaults/{id}/blobs` — crea una subida reanudable (`tus` core, ADR-0012).
async fn crear_upload(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path(vault_id): Path<i64>,
    headers: HeaderMap,
) -> Result<axum::response::Response> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;
    exigir_tus(&headers)?;

    // Barrido perezoso: acá es donde una subida abandonada libera su reserva de cuota (ADR-0012).
    let _ = crate::uploads::barrer(&state.pool, &state.config.cas_root).await?;

    let total = header_entero(&headers, UPLOAD_LENGTH)?;
    if total <= 0 {
        return Err(ServerError::Invalid(
            "Upload-Length debe ser mayor que cero".to_owned(),
        ));
    }

    // El techo del cuerpo (contenido + sobre) ya lo resuelve `blob_limit` (ADR-0014).
    let techo = blob_limit(&state.pool, vault_id).await?;
    if total > techo {
        return Err(ServerError::Protocol(
            ovaultd_protocol::ErrorCode::MaxFileExceeded,
        ));
    }

    let blob_id = parse_blob_metadata(&headers)?;

    let creada = crate::uploads::crear(
        &state.pool,
        vault_id,
        &blob_id,
        total,
        state.config.upload_ttl_hours,
    )
    .await?;

    let mut respuesta = HeaderMap::new();
    respuesta.insert(TUS_RESUMABLE, HeaderValue::from_static("1.0.0"));
    respuesta.insert(UPLOAD_LENGTH, header_i64(creada.total));
    respuesta.insert(UPLOAD_OFFSET, header_i64(creada.offset));
    respuesta.insert(
        LOCATION,
        HeaderValue::from_str(&format!(
            "/api/v1/vaults/{vault_id}/uploads/{}",
            creada.upload_id
        ))
        .map_err(|_| ServerError::Invalid("ubicación de la subida inválida".to_owned()))?,
    );

    Ok((
        StatusCode::CREATED,
        respuesta,
        Json(serde_json::json!({
            "upload_id": creada.upload_id,
            "offset": creada.offset,
            "total": creada.total,
        })),
    )
        .into_response())
}

/// `HEAD /api/v1/vaults/{id}/uploads/{upload_id}` — dónde reanudar (tus core).
async fn estado_upload(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path((vault_id, upload_id)): Path<(i64, String)>,
    headers: HeaderMap,
) -> Result<axum::response::Response> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;
    exigir_tus(&headers)?;

    let estado = crate::uploads::estado(&state.pool, vault_id, &upload_id)
        .await?
        .ok_or(ServerError::NotFound)?;

    let mut respuesta = HeaderMap::new();
    respuesta.insert(TUS_RESUMABLE, HeaderValue::from_static("1.0.0"));
    respuesta.insert(UPLOAD_LENGTH, header_i64(estado.total));
    respuesta.insert(UPLOAD_OFFSET, header_i64(estado.offset));

    Ok((StatusCode::OK, respuesta).into_response())
}

/// `PATCH /api/v1/vaults/{id}/uploads/{upload_id}` — sube un bloque (tus core, ADR-0012).
async fn parchear_upload(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path((vault_id, upload_id)): Path<(i64, String)>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<axum::response::Response> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;
    exigir_tus(&headers)?;

    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|valor| valor.to_str().ok());
    if content_type != Some(OFFSET_OCTET_STREAM) {
        return Err(ServerError::Invalid(
            "Content-Type debe ser application/offset+octet-stream".to_owned(),
        ));
    }

    let offset_pedido = header_entero(&headers, UPLOAD_OFFSET)?;
    if offset_pedido < 0 {
        return Err(ServerError::Invalid(
            "Upload-Offset no puede ser negativo".to_owned(),
        ));
    }

    let estado = crate::uploads::estado(&state.pool, vault_id, &upload_id)
        .await?
        .ok_or(ServerError::NotFound)?;

    // El tope de lectura es el total declarado (nada más grande se bufferea). Un cuerpo que no
    // se puede leer NO mata la subida: puede ser un reconnect cortado, y el punto es reanudar;
    // el exceso REAL sobre el offset confirmado lo detecta `uploads::aplicar` (terminal).
    let limite = usize::try_from(estado.total).unwrap_or(usize::MAX);
    let bytes = axum::body::to_bytes(request.into_body(), limite)
        .await
        .map_err(|_| {
            ServerError::Invalid(
                "no se pudo leer el bloque (¿excede Upload-Length?)".to_owned(),
            )
        })?;

    let aplicado = crate::uploads::aplicar(
        &state.pool,
        &state.cas,
        &state.config.cas_root,
        vault_id,
        &upload_id,
        offset_pedido,
        state.config.upload_ttl_hours,
        bytes.to_vec(),
    )
    .await?;

    let offset = match aplicado {
        crate::uploads::Aplicado::Parcial { offset }
        | crate::uploads::Aplicado::Completo { offset }
        | crate::uploads::Aplicado::YaCompleto { offset } => offset,
    };

    let mut respuesta = HeaderMap::new();
    respuesta.insert(TUS_RESUMABLE, HeaderValue::from_static("1.0.0"));
    respuesta.insert(UPLOAD_OFFSET, header_i64(offset));

    Ok((StatusCode::NO_CONTENT, respuesta).into_response())
}

async fn listar_devices(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
) -> Result<Json<Vec<DeviceResponse>>> {
    let devices = sqlx::query_as::<_, DeviceRow>(
        "SELECT id, name, platform, created_at, revoked_at
         FROM devices
         WHERE account_id = ?
         ORDER BY id",
    )
    .bind(autenticado.account_id)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(
        devices
            .into_iter()
            .map(|device| DeviceResponse {
                id: device.id,
                name: device.name,
                platform: device.platform,
                created_at: device.created_at,
                revoked_at: device.revoked_at,
            })
            .collect(),
    ))
}

#[derive(Debug, Deserialize)]
struct CreateDeviceRequest {
    name: String,
    #[serde(default)]
    platform: Option<String>,
}

async fn crear_device(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Json(request): Json<CreateDeviceRequest>,
) -> Result<(StatusCode, Json<Value>)> {
    let token = auth::emitir_device(
        &state.pool,
        autenticado.account_id,
        &request.name,
        request.platform.as_deref(),
    )
    .await?;

    let device_id = auth::verificar_token(&state.pool, &token).await?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "device_id": device_id,
            "token": token
        })),
    ))
}

async fn revocar_device(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path(device_id): Path<i64>,
) -> Result<StatusCode> {
    let owner = sqlx::query_scalar::<_, i64>(
        "SELECT id
         FROM devices
         WHERE id = ? AND account_id = ?",
    )
    .bind(device_id)
    .bind(autenticado.account_id)
    .fetch_optional(&state.pool)
    .await?;

    if owner.is_none() {
        return Err(ServerError::NotFound);
    }

    auth::revocar_device(&state.pool, device_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{
        Form, FromRequestParts, Json, Path, Query, Request, State,
    },
    http::{
        header::{AUTHORIZATION, CONTENT_RANGE, CONTENT_TYPE, HOST, LOCATION, RANGE},
        request::Parts,
        HeaderMap, HeaderValue, Method, StatusCode,
    },
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, head, post, put},
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool};
use ovaultd_protocol::BlobId;
use tower_http::cors::{Any, CorsLayer};

use askama::Template;

use crate::{
    admin::{panel, textos::Textos},
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
    /// Freno de intentos del login del panel (ADR-0017): en memoria, single-node por diseño.
    pub limitador_login: Arc<crate::admin::sesiones::LimitadorDeLogin>,
    /// Señal de "hay revisiones" para los clientes conectados por WebSocket. En proceso (single
    /// node por diseño): no es fuente de verdad, el cliente relee el oplog. Ver `storage::Notifier`.
    pub notifier: Arc<crate::storage::Notifier>,
}

pub fn router(estado: EstadoServidor) -> Router {
    // Marca de arranque para `/metrics` (idempotente: el primer router montado manda).
    let _ = ARRANQUE.set(std::time::Instant::now());

    // El middleware de la IP del cliente lleva su propia copia del estado (el router se
    // queda con la otra en `with_state`).
    let estado_para_ip = estado.clone();

    // F-09: con `metrics` apagado la ruta no existe (404). En `localhost`, el handler exige
    // que el peer TCP sea loopback; en `on`, queda como el resto del API.
    let router = Router::new().route("/healthz", get(healthz));
    let router = if estado.config.metrics == crate::config::ModoMetrics::Off {
        router
    } else {
        router.route("/metrics", get(metrics))
    };

    router
        .route("/", get(raiz))
        .route("/setup", get(setup_form).post(setup_crear))
        .merge(crate::admin::rutas())
        .route("/api/v1/meta", get(meta))
        .route("/api/v1/auth/signin", post(signin))
        .route("/api/v1/auth/signout", post(signout))
        .route("/api/v1/me", get(me))
        .route("/api/v1/vaults", get(listar_vaults).post(crear_vault))
        .route(
            "/api/v1/vaults/{id}/material",
            get(material_vault).put(actualizar_material),
        )
        .route("/api/v1/vaults/{id}", delete(borrar_vault))
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
            "/api/v1/vaults/{id}/history",
            get(historial),
        )
        .route(
            "/api/v1/vaults/{id}/snapshots",
            get(listar_snapshots).post(crear_snapshot),
        )
        .route(
            "/api/v1/vaults/{id}/snapshots/{snap}",
            get(obtener_snapshot).delete(borrar_snapshot),
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
        // La IP del cliente (peer TCP; headers SOLO con `trust_proxy`) queda anotada para
        // todos los handlers: el freno de login y la auditoría dejan de creerle a cualquiera.
        .layer(axum::middleware::from_fn_with_state(
            estado_para_ip,
            crate::peer::resolver_ip_de_cliente,
        ))
        .layer(
            // CORS para los clientes browser (runtime del núcleo / plugin de Obsidian):
            // hacen fetch cross-origin al server. El API es token-based (sin cookies):
            // el origen comodín no habilita CSRF, y sin esto el fetch queda bloqueado
            // (el upgrade del WS no pasa por CORS y por eso no lo cubre).
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::PATCH,
                    Method::DELETE,
                    Method::HEAD,
                ])
                .allow_headers([
                    AUTHORIZATION,
                    CONTENT_TYPE,
                    // El cliente se identifica por header (ADR-0018): si no está permitido
                    // acá, el preflight lo bloquea y el plugin no llega ni a vincular.
                    axum::http::header::HeaderName::from_static("x-ovaultd-client"),
                ]),
        )
}

/// Marca de arranque del proceso para `/metrics` (la fija el primer router montado).
static ARRANQUE: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Segundos desde el arranque del proceso (lo comparten `/metrics` y el hero del panel).
pub(crate) fn uptime_segundos() -> u64 {
    ARRANQUE
        .get()
        .map(|inicio| inicio.elapsed().as_secs())
        .unwrap_or(0)
}

/// Versión mínima del plugin con la que este server declara compatibilidad (ADR-0018:
/// release train, mismo `MAJOR.MINOR` = pareja probada junta).
const MIN_PLUGIN_VERSION: &str = "0.2.0";

/// Features que este server anuncia en `/meta`. Los nombres son CONTRATO: un cliente
/// decide capacidades por acá, no por número de versión.
const FEATURES: [&str; 6] = [
    "meta",
    "admin-web",
    "resumable-uploads",
    "vault-delete",
    "vault-material",
    "device-revoke",
];

/// Cuerpo de `GET /api/v1/meta` (ADR-0018).
#[derive(Serialize)]
struct Meta {
    server_version: &'static str,
    api: &'static str,
    wire_protocol: &'static str,
    min_plugin_version: &'static str,
    features: &'static [&'static str],
}

/// `GET /api/v1/meta` — el contrato de compatibilidad. **Sin auth**: es exactamente lo
/// que un cliente consulta ANTES de vincular, y el server sabe defenderse solo.
async fn meta() -> Json<Meta> {
    Json(Meta {
        server_version: env!("CARGO_PKG_VERSION"),
        api: ovaultd_protocol::API_VERSION,
        wire_protocol: ovaultd_protocol::WIRE_PROTOCOL,
        min_plugin_version: MIN_PLUGIN_VERSION,
        features: &FEATURES,
    })
}

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

/// Cuentas del servidor. El bootstrap por web (`/setup`) depende de que este número sea cero.
async fn contar_cuentas(pool: &SqlitePool) -> Result<i64> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM accounts")
        .fetch_one(pool)
        .await?)
}

/// ¿El pedido de setup trae la autorización correcta? Sin `OVAULTD_SETUP_TOKEN` el formulario
/// queda abierto (existe solo mientras no haya cuentas); con token, se exige.
fn setup_autorizado(config: &Config, token: Option<&str>) -> bool {
    match config.setup_token.as_deref() {
        None => true,
        Some(esperado) => token
            .map(|recibido| auth::tokens_iguales(recibido, esperado))
            .unwrap_or(false),
    }
}

// `tokens_iguales` vive en `auth` (una sola fuente en tiempo constante): la comparten el
// setup_token del bootstrap y el CSRF del panel (ADR-0017).

/// `GET /` — raíz web: sin cuentas lleva al bootstrap; con cuentas, página mínima.
async fn raiz(State(state): State<EstadoServidor>) -> Result<Response> {
    if contar_cuentas(&state.pool).await? == 0 {
        return Ok(Redirect::to("/setup").into_response());
    }

    // Con cuentas, el server tiene cara de panel (login/dashboard).
    Ok(Redirect::to("/admin").into_response())
}

#[derive(Deserialize)]
struct ConsultaSetup {
    #[serde(default)]
    token: Option<String>,
}

#[derive(Deserialize)]
struct FormularioSetup {
    email: String,
    password: String,
    #[serde(default)]
    token: Option<String>,
}

/// La pantalla del bootstrap. Los textos salen de `textos::t()` (la regla de i18n del panel: ningún
/// string visible vive fuera de `admin/textos.rs`) y el CSS entra por `base_centrado.html`, el mismo
/// de login/register: la primera pantalla que ve un usuario tiene que ser la del producto.
#[derive(Template)]
#[template(path = "admin/setup.html")]
struct PlantillaSetup {
    t: &'static Textos,
    token: Option<String>,
    cerrado: bool,
    token_falta: bool,
    error: Option<String>,
    hecho: bool,
}

impl PlantillaSetup {
    fn nueva() -> Self {
        Self {
            t: textos(),
            token: None,
            cerrado: false,
            token_falta: false,
            error: None,
            hecho: false,
        }
    }
}

/// Renderiza el bootstrap preservando el status (410 cerrado, 403 sin token, 400 datos inválidos):
/// los códigos son parte del contrato, no un detalle del cuerpo.
fn render_setup(status: StatusCode, plantilla: PlantillaSetup) -> Response {
    let mut respuesta = panel::render(|| plantilla.render(), "setup");
    *respuesta.status_mut() = status;
    respuesta
}

fn textos() -> &'static Textos {
    crate::admin::textos::t()
}

/// `GET /setup` — bootstrap del primer usuario (ADR-0016): existe solo mientras el servidor no
/// tenga NINGUNA cuenta. Con cuentas, 410 Gone para siempre.
async fn setup_form(
    State(state): State<EstadoServidor>,
    Query(consulta): Query<ConsultaSetup>,
) -> Result<Response> {
    if contar_cuentas(&state.pool).await? > 0 {
        return Ok(render_setup(
            StatusCode::GONE,
            PlantillaSetup {
                cerrado: true,
                ..PlantillaSetup::nueva()
            },
        ));
    }

    let token = consulta.token.clone();
    if !setup_autorizado(&state.config, token.as_deref()) {
        return Ok(render_setup(
            StatusCode::FORBIDDEN,
            PlantillaSetup {
                token_falta: true,
                ..PlantillaSetup::nueva()
            },
        ));
    }

    Ok(render_setup(
        StatusCode::OK,
        PlantillaSetup {
            token,
            ..PlantillaSetup::nueva()
        },
    ))
}

/// `POST /setup` — crea la primera cuenta. La condición "no hay ninguna" viaja DENTRO del
/// INSERT (`crear_cuenta_si_vacio`): dos envíos simultáneos no pueden crear dos cuentas.
async fn setup_crear(
    State(state): State<EstadoServidor>,
    Form(formulario): Form<FormularioSetup>,
) -> Result<Response> {
    if contar_cuentas(&state.pool).await? > 0 {
        return Ok(render_setup(
            StatusCode::GONE,
            PlantillaSetup {
                cerrado: true,
                ..PlantillaSetup::nueva()
            },
        ));
    }

    if !setup_autorizado(&state.config, formulario.token.as_deref()) {
        return Ok(render_setup(
            StatusCode::FORBIDDEN,
            PlantillaSetup {
                token_falta: true,
                ..PlantillaSetup::nueva()
            },
        ));
    }

    if formulario.email.trim().is_empty() || !formulario.email.contains('@') || formulario.email.len() > 254 {
        return Ok(render_setup(
            StatusCode::BAD_REQUEST,
            PlantillaSetup {
                error: Some(textos().setup_email_invalido.to_string()),
                ..PlantillaSetup::nueva()
            },
        ));
    }

    if formulario.password.chars().count() < 8 {
        return Ok(render_setup(
            StatusCode::BAD_REQUEST,
            PlantillaSetup {
                error: Some(textos().setup_password_corta.to_string()),
                ..PlantillaSetup::nueva()
            },
        ));
    }

    let email = formulario.email.trim().to_string();

    match auth::crear_cuenta_si_vacio(&state.pool, &email, &formulario.password).await {
        Ok(cuenta) => {
            tracing::info!(cuenta = cuenta.id, "bootstrap: primera cuenta creada por /setup");
            Ok(render_setup(
                StatusCode::OK,
                PlantillaSetup {
                    hecho: true,
                    ..PlantillaSetup::nueva()
                },
            ))
        }
        // Carrera perdida: otro POST creó la cuenta entre el conteo y el INSERT.
        Err(ServerError::Invalid(_)) => Ok(render_setup(
            StatusCode::GONE,
            PlantillaSetup {
                cerrado: true,
                ..PlantillaSetup::nueva()
            },
        )),
        Err(error) => Err(error),
    }
}

/// `GET /metrics` — Prometheus mínimo y SOLO de operación: ni secretos ni payloads.
///
/// El modo sale de la config (`OVAULTD_METRICS`): en `localhost`, responde solo al peer TCP
/// de loopback (el acceso directo al puerto; un request que llegó por el borde no lo es,
/// aunque su `X-Forwarded-For` diga `127.0.0.1`); en `on`, queda expuesto como el resto del
/// API y conviene restringirlo en el borde (`location /metrics { deny all; }`, ver
/// `docs/deploy.md`).
async fn metrics(
    State(estado): State<EstadoServidor>,
    headers: HeaderMap,
) -> axum::response::Response {
    if estado.config.metrics == crate::config::ModoMetrics::Localhost
        && !crate::peer::peer_de(&headers)
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
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
        .into_response()
}

pub struct Autenticado {
    pub account_id: i64,
    pub device_id: i64,
}

/// Resuelve un token de device a `(account_id, device_id)`. Fuente ÚNICA para el
/// extractor HTTP y para la autenticación del upgrade del WebSocket (ADR-0015).
pub(crate) async fn autenticar_token(
    state: &EstadoServidor,
    token: &str,
) -> std::result::Result<(i64, i64), ServerError> {
    let device_id = auth::verificar_token(&state.pool, token)
        .await
        .map_err(|_| ServerError::Unauthorized)?;

    let account_id = sqlx::query_scalar::<_, i64>(
        "SELECT d.account_id
         FROM devices d
         JOIN accounts a ON a.id = d.account_id
         WHERE d.id = ? AND a.disabled_at IS NULL",
    )
    .bind(device_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(ServerError::Unauthorized)?;

    Ok((account_id, device_id))
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

        let (account_id, device_id) = autenticar_token(state, token).await?;

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
    headers: HeaderMap,
    Json(request): Json<SigninRequest>,
) -> Result<Json<SigninResponse>> {
    if request.mfa.is_some() {
        return Err(ServerError::Invalid(
            "mfa no implementado: no mandes el código, no se está verificando".to_owned(),
        ));
    }

    // Freno de fuerza bruta (mismo limitador y misma clave `ip|email` que el panel web).
    // La IP ya es confiable (peer TCP; headers solo con `trust_proxy`): rotar
    // `X-Forwarded-For` ya no esquiva el freno.
    let ip = crate::peer::ip_de(&headers);
    let clave = format!(
        "{}|{}",
        ip.as_deref().unwrap_or("-"),
        request.email.trim().to_ascii_lowercase()
    );

    if !state.limitador_login.permitir(&clave) {
        crate::admin::sesiones::auditar(
            &state.pool,
            None,
            "login_frenado",
            Some("demasiados intentos de signin"),
            ip.as_deref(),
        )
        .await;

        return Err(ServerError::Frenado);
    }

    match auth::iniciar_sesion(
        &state.pool,
        &request.email,
        &request.password,
        request.name.as_deref().unwrap_or("default"),
        request.platform.as_deref(),
    )
    .await
    {
        Ok((token, device_id)) => {
            state.limitador_login.limpiar(&clave);
            Ok(Json(SigninResponse { token, device_id }))
        }
        Err(error) => {
            // Cualquier fallo de credenciales cuenta para la ventana — la cuenta
            // INEXISTENTE incluida (es el caso que más prueba un password spraying); un
            // error interno del server no (no es señal de fuerza bruta).
            if !matches!(error, ServerError::Db(_) | ServerError::Io(_)) {
                state.limitador_login.registrar(&clave);
            }
            Err(error)
        }
    }
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
    headers: HeaderMap,
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
    .bind(&request.name)
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

    // El alta queda auditada (el panel la muestra en `/admin/auditoria`).
    crate::admin::sesiones::auditar(
        &state.pool,
        Some(autenticado.account_id),
        "vault_created",
        Some(&format!("#{id} «{}»", request.name)),
        crate::admin::panel::ip_del_cliente(&headers).as_deref(),
    )
    .await;

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

/// Confirmación de borrado: el nombre EXACTO del vault (no un booleano — un DELETE
/// accidental sin leer el prompt no lo pasa).
#[derive(Debug, Deserialize)]
struct ConfirmacionBorrado {
    confirm: Option<String>,
}

/// Núcleo del borrado: cascada en la DB + barrido del CAS de los blobs sin referencias en
/// ningún otro vault. Devuelve cuántos blobs físicos se barrieron. Lo comparten la API y
/// el panel (W1: borrado forzado de una cuenta con vaults).
pub(crate) async fn purgar_vault(
    pool: &SqlitePool,
    cas: &crate::storage::CasStore,
    vault_id: i64,
) -> Result<usize> {
    // Los hashes del vault ANTES del DELETE (la cascada se lleva la tabla `blobs`).
    let hashes: Vec<Vec<u8>> = sqlx::query_scalar("SELECT hash FROM blobs WHERE vault_id = ?")
        .bind(vault_id)
        .fetch_all(pool)
        .await?;

    sqlx::query("DELETE FROM vaults WHERE id = ?")
        .bind(vault_id)
        .execute(pool)
        .await?;

    // Barrido del CAS: solo los blobs que ya no referencia ningún vault.
    let mut barridos = 0usize;
    for hash in hashes {
        let referencias: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM blobs WHERE hash = ?")
            .bind(&hash)
            .fetch_one(pool)
            .await?;
        if referencias == 0 {
            if let Ok(bytes) = <[u8; 32]>::try_from(hash.as_slice()) {
                cas.remove(&BlobId::new(bytes)).await?;
                barridos += 1;
            }
        }
    }
    Ok(barridos)
}

/// `DELETE /api/v1/vaults/{id}?confirm=<nombre>` — borrado REAL de un vault de la cuenta.
///
/// La cascada de la DB (`ON DELETE CASCADE` + `foreign_keys(true)`) limpia devices, files,
/// revisions, batches, blobs del índice, uploads y challenges; después se barren del CAS los
/// blobs que no quedaron referenciados por NINGÚN otro vault. Queda auditado.
async fn borrar_vault(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path(vault_id): Path<i64>,
    Query(consulta): Query<ConfirmacionBorrado>,
) -> Result<StatusCode> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    let nombre: String = sqlx::query_scalar("SELECT name FROM vaults WHERE id = ?")
        .bind(vault_id)
        .fetch_one(&state.pool)
        .await?;

    let confirmacion = consulta.confirm.unwrap_or_default();
    if confirmacion != nombre {
        return Err(ServerError::Invalid(
            "confirmá el borrado con el nombre exacto del vault (parámetro `confirm`)".to_owned(),
        ));
    }

    let barridos = purgar_vault(&state.pool, &state.cas, vault_id).await?;

    tracing::info!(vault = vault_id, %nombre, barridos, "vault borrado");
    sqlx::query(
        "INSERT INTO audit_log (actor_account_id, action, detail) VALUES (?, 'vault_delete', ?)",
    )
    .bind(autenticado.account_id)
    .bind(format!("#{vault_id} «{nombre}» ({barridos} blobs del CAS)"))
    .execute(&state.pool)
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
struct ActualizarMaterialRequest {
    /// `wrapped_key_pw` nuevo (base64): la `vault_key` re-envuelta con la password nueva.
    wrapped_key_pw: String,
}

/// `PUT /api/v1/vaults/{id}/material` — re-sella la envuelta de la password: lo usan el
/// cambio de password del vault y la recuperación con la recovery key. E2EE: el server solo
/// guarda el ciphertext nuevo; no puede —ni necesita— verificar más allá del formato.
async fn actualizar_material(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path(vault_id): Path<i64>,
    Json(cuerpo): Json<ActualizarMaterialRequest>,
) -> Result<Json<Value>> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    let wrapped = STANDARD
        .decode(cuerpo.wrapped_key_pw.trim())
        .map_err(|_| ServerError::Invalid("wrapped_key_pw no es base64 válido".to_owned()))?;
    // nonce (24) + ciphertext mínimo de la vault_key (32): más corto que eso no es una envuelta.
    if wrapped.len() < 24 + 32 {
        return Err(ServerError::Invalid(
            "wrapped_key_pw es demasiado corto para una envuelta válida".to_owned(),
        ));
    }

    sqlx::query("UPDATE vaults SET wrapped_key_pw = ? WHERE id = ?")
        .bind(&wrapped)
        .bind(vault_id)
        .execute(&state.pool)
        .await?;

    tracing::info!(vault = vault_id, "material: wrapped_key_pw actualizado");
    sqlx::query(
        "INSERT INTO audit_log (actor_account_id, action, detail) VALUES (?, 'vault_rekey', ?)",
    )
    .bind(autenticado.account_id)
    .bind(format!("#{vault_id}: envuelta de password actualizada"))
    .execute(&state.pool)
    .await?;

    Ok(Json(serde_json::json!({ "ok": true })))
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

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    path_id: String,
    #[serde(default)]
    before: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

async fn historial(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path(vault_id): Path<i64>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<crate::history::Historial>> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    let path_id = parse_hex_32(&query.path_id, "path_id")?;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);

    Ok(Json(
        crate::history::listar(&state.pool, vault_id, &path_id, query.before, limit).await?,
    ))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SnapshotRequest {
    rev: Option<i64>,
    label: Option<String>,
}

async fn crear_snapshot(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path(vault_id): Path<i64>,
    cuerpo: Option<Json<SnapshotRequest>>,
) -> Result<(StatusCode, Json<crate::history::Snapshot>)> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    let cuerpo = cuerpo.map(|Json(cuerpo)| cuerpo).unwrap_or_default();

    let snapshot =
        crate::history::crear_snapshot(&state.pool, vault_id, cuerpo.rev, cuerpo.label).await?;

    Ok((StatusCode::CREATED, Json(snapshot)))
}

async fn listar_snapshots(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path(vault_id): Path<i64>,
) -> Result<Json<Vec<crate::history::Snapshot>>> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    Ok(Json(
        crate::history::listar_snapshots(&state.pool, vault_id).await?,
    ))
}

async fn obtener_snapshot(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path((vault_id, snap)): Path<(i64, i64)>,
) -> Result<Json<crate::history::Snapshot>> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    Ok(Json(
        crate::history::obtener_snapshot(&state.pool, vault_id, snap).await?,
    ))
}

async fn borrar_snapshot(
    State(state): State<EstadoServidor>,
    autenticado: Autenticado,
    Path((vault_id, snap)): Path<(i64, i64)>,
) -> Result<StatusCode> {
    resolve_vault(&state.pool, autenticado.account_id, vault_id).await?;

    if crate::history::borrar_snapshot(&state.pool, vault_id, snap).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ServerError::NotFound)
    }
}

fn parse_hex_32(value: &str, etiqueta: &str) -> Result<[u8; 32]> {
    if value.len() != 64
        || !value
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Err(ServerError::Invalid(format!(
            "{etiqueta} debe ser hexadecimal minúsculo de 64 caracteres"
        )));
    }

    let mut bytes = [0u8; 32];

    for index in 0..32 {
        bytes[index] = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| ServerError::Invalid(format!("{etiqueta} inválido")))?;
    }

    Ok(bytes)
}

fn parse_blob_hash(value: &str) -> Result<BlobId> {
    parse_hex_32(value, "el hash").map(BlobId::new)
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
        crate::failpoints::failpoint("ack.put_blob");
        return Ok(StatusCode::OK);
    }

    state.cas.publish(&blob_id, &bytes).await?;

    // El índice primero que la referencia: `commit_batch` rechaza (`not_found`) un batch que
    // apunte a un blob sin fila en `blobs`.
    crate::storage::registrar_blob(&state.pool, vault_id, &blob_id, size).await?;

    // Failpoint de test: crash después del registro y antes del ACK HTTP.
    crate::failpoints::failpoint("ack.put_blob");

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

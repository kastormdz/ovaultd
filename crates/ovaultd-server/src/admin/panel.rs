//! Páginas y handlers del panel de administración (ADR-0017).
//!
//! Server-rendered con askama (templates compilados dentro del binario) y htmx vendorizado
//! (`assets/htmx.min.js`, sin CDNs). El panel funciona igual sin JS: los formularios son
//! POST clásicos y htmx solo agrega comodidad.
//!
//! Autorización: `/admin` exige sesión (cookie opaca). Cada POST exige además el token CSRF
//! de la sesión (tiempo constante). Las páginas autenticadas van con `cache-control: no-store`.

use askama::Template;
use axum::extract::{FromRequestParts, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, SET_COOKIE, USER_AGENT};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;

use crate::admin::sesiones::{self, CAMPO_CSRF, COOKIE_SESION, SESION_TTL_DIAS};
use crate::auth;
use crate::config::ModoSecureCookies;
use crate::routes::EstadoServidor;

const HTMX_JS: &str = include_str!("../../assets/htmx.min.js");
const ADMIN_CSS: &str = include_str!("../../assets/admin.css");
const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn rutas() -> Router<EstadoServidor> {
    Router::new()
        .route("/admin", get(dashboard))
        .route("/admin/vaults", get(vaults))
        .route("/admin/login", get(login_form).post(login_post))
        .route("/admin/logout", post(logout))
        .route("/admin/mi-cuenta", get(mi_cuenta_form).post(mi_cuenta_post))
        .route("/admin/assets/admin.css", get(asset_css))
        .route("/admin/assets/htmx.min.js", get(asset_htmx))
}

// ------------------------------------------------------------------ extractor de sesión

/// La sesión web del panel. Sin cookie válida no hay handler: redirect a `/admin/login`.
pub struct Sesion(pub sesiones::SesionActiva);

impl FromRequestParts<EstadoServidor> for Sesion {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        estado: &EstadoServidor,
    ) -> std::result::Result<Self, Self::Rejection> {
        let token = cookie_de(&parts.headers, COOKIE_SESION)
            .ok_or_else(|| Redirect::to("/admin/login").into_response())?;

        match sesiones::verificar_sesion(&estado.pool, &token).await {
            Ok(Some(sesion)) => Ok(Sesion(sesion)),
            _ => Err(Redirect::to("/admin/login").into_response()),
        }
    }
}

// ------------------------------------------------------------------ helpers HTTP

/// Lee una cookie del header `Cookie` (formato `a=1; b=2`).
fn cookie_de(headers: &HeaderMap, nombre: &str) -> Option<String> {
    let crudo = headers.get(COOKIE)?.to_str().ok()?;
    for par in crudo.split(';') {
        if let Some((clave, valor)) = par.trim().split_once('=') {
            if clave == nombre {
                return Some(valor.to_owned());
            }
        }
    }
    None
}

/// ¿Este request llegó por HTTPS? (el borde setea `X-Forwarded-Proto`).
fn es_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|valor| valor.to_str().ok())
        .map(|valor| valor.eq_ignore_ascii_case("https"))
        .unwrap_or(false)
}

fn cookie_segura(estado: &EstadoServidor, headers: &HeaderMap) -> bool {
    match estado.config.secure_cookies {
        ModoSecureCookies::Siempre => true,
        ModoSecureCookies::Nunca => false,
        ModoSecureCookies::Auto => es_https(headers),
    }
}

fn cookie_sesion(token: &str, segura: bool) -> String {
    let mut cookie = format!(
        "{COOKIE_SESION}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        SESION_TTL_DIAS * 24 * 60 * 60
    );
    if segura {
        cookie.push_str("; Secure");
    }
    cookie
}

fn cookie_expirada(segura: bool) -> String {
    let mut cookie = format!("{COOKIE_SESION}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if segura {
        cookie.push_str("; Secure");
    }
    cookie
}

/// IP del cliente tal como la ve el borde (`X-Real-IP` o el primer `X-Forwarded-For`).
fn ip_del_cliente(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-real-ip")
        .or_else(|| headers.get("x-forwarded-for"))
        .and_then(|valor| valor.to_str().ok())
        .map(|valor| valor.split(',').next().unwrap_or(valor).trim().to_owned())
        .filter(|ip| !ip.is_empty())
}

fn user_agent_de(headers: &HeaderMap) -> Option<&str> {
    headers.get(USER_AGENT).and_then(|valor| valor.to_str().ok())
}

/// Página autenticada: nada se cachea.
fn respuesta_html(html: String) -> Response {
    ([(CACHE_CONTROL, "no-store")], Html(html)).into_response()
}

fn error_interno(contexto: &str, error: impl std::fmt::Display) -> Response {
    tracing::error!(contexto, %error, "error en el panel");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(CACHE_CONTROL, "no-store")],
        Html("<h1>500</h1><p>Error interno. Mirá los logs del server.</p>".to_owned()),
    )
        .into_response()
}

fn render<F>(renderizar: F, contexto: &str) -> Response
where
    F: FnOnce() -> std::result::Result<String, askama::Error>,
{
    match renderizar() {
        Ok(html) => respuesta_html(html),
        Err(error) => error_interno(contexto, error),
    }
}

// ------------------------------------------------------------------ plantillas

#[derive(Template)]
#[template(path = "admin/login.html")]
struct PlantillaLogin {
    version: String,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/dashboard.html")]
struct PlantillaDashboard {
    version: String,
    email: String,
    rol: String,
    csrf: String,
    cuentas: i64,
    vaults: i64,
    notas: i64,
    revisiones: i64,
    devices: i64,
    espacio: String,
}

/// Vista de un vault para la tabla: SOLO lo que el server puede ver (es ciego a los
/// nombres de nota y al contenido — eso es de los clientes).
#[derive(Template)]
#[template(path = "admin/vaults.html")]
struct PlantillaVaults {
    version: String,
    email: String,
    csrf: String,
    vaults: Vec<VistaVault>,
}

struct VistaVault {
    id: i64,
    name: String,
    notas: i64,
    revisiones: i64,
    espacio: String,
    ultima_revision: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/mi_cuenta.html")]
struct PlantillaMiCuenta {
    version: String,
    email: String,
    csrf: String,
    error: Option<String>,
    ok: Option<String>,
}

// ------------------------------------------------------------------ login / logout

async fn login_form(State(estado): State<EstadoServidor>, headers: HeaderMap) -> Response {
    // Ya con sesión: directo al panel.
    if let Some(token) = cookie_de(&headers, COOKIE_SESION) {
        if let Ok(Some(_)) = sesiones::verificar_sesion(&estado.pool, &token).await {
            return Redirect::to("/admin").into_response();
        }
    }

    render(
        || {
            PlantillaLogin {
                version: VERSION.to_owned(),
                error: None,
            }
            .render()
        },
        "login_form",
    )
}

#[derive(Debug, Deserialize)]
struct FormLogin {
    email: String,
    password: String,
}

async fn login_post(
    State(estado): State<EstadoServidor>,
    headers: HeaderMap,
    Form(formulario): Form<FormLogin>,
) -> Response {
    let ip = ip_del_cliente(&headers);
    let clave = format!(
        "{}|{}",
        ip.as_deref().unwrap_or("-"),
        formulario.email.trim().to_ascii_lowercase()
    );

    if !estado.limitador_login.permitir(&clave) {
        sesiones::auditar(
            &estado.pool,
            None,
            "login_frenado",
            Some("demasiados intentos"),
            ip.as_deref(),
        )
        .await;

        return (
            StatusCode::TOO_MANY_REQUESTS,
            [
                (CACHE_CONTROL, "no-store"),
                (axum::http::header::RETRY_AFTER, "60"),
            ],
            Html(
                "<h1>Demasiados intentos</h1><p>Probá de nuevo en un minuto.</p>".to_owned(),
            ),
        )
            .into_response();
    }

    match auth::autenticar_cuenta(&estado.pool, &formulario.email, &formulario.password).await {
        Ok(Some(cuenta)) => {
            estado.limitador_login.limpiar(&clave);
            let _ = sesiones::limpiar_vencidas(&estado.pool).await;

            match sesiones::crear_sesion(
                &estado.pool,
                cuenta.id,
                ip.as_deref(),
                user_agent_de(&headers),
            )
            .await
            {
                Ok(sesion) => {
                    sesiones::auditar(&estado.pool, Some(cuenta.id), "login_ok", None, ip.as_deref())
                        .await;

                    let cookie = cookie_sesion(&sesion.token, cookie_segura(&estado, &headers));
                    let mut respuesta = Redirect::to("/admin").into_response();
                    if let Ok(valor) = HeaderValue::from_str(&cookie) {
                        respuesta.headers_mut().insert(SET_COOKIE, valor);
                    }
                    respuesta
                }
                Err(error) => error_interno("crear_sesion", error),
            }
        }
        Ok(None) => {
            estado.limitador_login.registrar(&clave);
            sesiones::auditar(
                &estado.pool,
                None,
                "login_fallido",
                Some(&formulario.email.trim().to_ascii_lowercase()),
                ip.as_deref(),
            )
            .await;

            match (PlantillaLogin {
                version: VERSION.to_owned(),
                error: Some("Email o password incorrectos.".to_owned()),
            })
            .render()
            {
                Ok(html) => (
                    StatusCode::UNAUTHORIZED,
                    [(CACHE_CONTROL, "no-store")],
                    Html(html),
                )
                    .into_response(),
                Err(error) => error_interno("login_fallido", error),
            }
        }
        Err(error) => error_interno("autenticar_cuenta", error),
    }
}

#[derive(Debug, Deserialize)]
struct FormConCsrf {
    #[serde(default)]
    csrf: String,
}

async fn logout(
    State(estado): State<EstadoServidor>,
    headers: HeaderMap,
    Sesion(sesion): Sesion,
    Form(formulario): Form<FormConCsrf>,
) -> Response {
    if !sesiones::csrf_valido(&sesion, &formulario.csrf) {
        return (StatusCode::FORBIDDEN, "CSRF inválido").into_response();
    }

    if let Some(token) = cookie_de(&headers, COOKIE_SESION) {
        let _ = sesiones::revocar_sesion(&estado.pool, &token).await;
    }
    sesiones::auditar(
        &estado.pool,
        Some(sesion.account_id),
        "logout",
        None,
        ip_del_cliente(&headers).as_deref(),
    )
    .await;

    let mut respuesta = Redirect::to("/admin/login").into_response();
    if let Ok(valor) = HeaderValue::from_str(&cookie_expirada(cookie_segura(&estado, &headers))) {
        respuesta.headers_mut().insert(SET_COOKIE, valor);
    }
    respuesta
}

// ------------------------------------------------------------------ dashboard

async fn dashboard(State(estado): State<EstadoServidor>, Sesion(sesion): Sesion) -> Response {
    let cuentas = contar(&estado.pool, "SELECT COUNT(*) FROM accounts").await;
    let vaults = contar(&estado.pool, "SELECT COUNT(*) FROM vaults").await;
    let notas = contar(
        &estado.pool,
        "SELECT COUNT(*) FROM files WHERE deleted = 0",
    )
    .await;
    let revisiones = contar(&estado.pool, "SELECT COUNT(*) FROM revisions").await;
    let devices = contar(
        &estado.pool,
        "SELECT COUNT(*) FROM devices WHERE revoked_at IS NULL",
    )
    .await;
    let bytes: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(size), 0) FROM blobs")
        .fetch_one(&estado.pool)
        .await
        .unwrap_or(0);

    render(
        || {
            PlantillaDashboard {
                version: VERSION.to_owned(),
                email: sesion.email.clone(),
                rol: sesion.role.clone(),
                csrf: sesion.csrf_token.clone(),
                cuentas,
                vaults,
                notas,
                revisiones,
                devices,
                espacio: formato_bytes(bytes),
            }
            .render()
        },
        "dashboard",
    )
}

// ------------------------------------------------------------------ vaults (stats)

#[derive(Debug, sqlx::FromRow)]
struct FilaVault {
    id: i64,
    name: String,
    notas: i64,
    revisiones: i64,
    bytes: i64,
    ultima_revision: Option<String>,
}

/// Estadísticas por vault: notas vivas (`files` sin borrar), revisiones (histórico),
/// espacio de los blobs cifrados y última actividad. El server NO tiene los nombres.
async fn vaults(State(estado): State<EstadoServidor>, Sesion(sesion): Sesion) -> Response {
    let filas = sqlx::query_as::<_, FilaVault>(
        "SELECT v.id, v.name,
                (SELECT COUNT(*) FROM files f WHERE f.vault_id = v.id AND f.deleted = 0) AS notas,
                (SELECT COUNT(*) FROM revisions r WHERE r.vault_id = v.id) AS revisiones,
                (SELECT COALESCE(SUM(b.size), 0) FROM blobs b WHERE b.vault_id = v.id) AS bytes,
                (SELECT MAX(r.ts) FROM revisions r WHERE r.vault_id = v.id) AS ultima_revision
         FROM vaults v
         ORDER BY v.id",
    )
    .fetch_all(&estado.pool)
    .await;

    let filas = match filas {
        Ok(filas) => filas,
        Err(error) => return error_interno("vaults", error),
    };

    let vista = filas
        .into_iter()
        .map(|fila| VistaVault {
            id: fila.id,
            name: fila.name,
            notas: fila.notas,
            revisiones: fila.revisiones,
            espacio: formato_bytes(fila.bytes),
            ultima_revision: fila.ultima_revision,
        })
        .collect();

    render(
        || {
            PlantillaVaults {
                version: VERSION.to_owned(),
                email: sesion.email.clone(),
                csrf: sesion.csrf_token.clone(),
                vaults: vista,
            }
            .render()
        },
        "vaults",
    )
}

/// Tamaño legible para humanos (los bytes del CAS: ciphertext — el server no ve más).
fn formato_bytes(bytes: i64) -> String {
    const UNIDADES: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut valor = bytes as f64;
    let mut unidad = 0;
    while valor >= 1024.0 && unidad < UNIDADES.len() - 1 {
        valor /= 1024.0;
        unidad += 1;
    }

    if unidad == 0 {
        format!("{bytes} B")
    } else {
        format!("{valor:.1} {}", UNIDADES[unidad])
    }
}

/// Contador suelto: si falla, cero (el dashboard nunca tumba el panel por un número).
/// `&'static str`: sqlx 0.9 exige consultas literales (SqlSafeStr) — con los call sites
/// de siempre alcanza.
async fn contar(pool: &sqlx::SqlitePool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap_or(0)
}

// ------------------------------------------------------------------ mi cuenta

async fn mi_cuenta_form(Sesion(sesion): Sesion) -> Response {
    render(
        || {
            PlantillaMiCuenta {
                version: VERSION.to_owned(),
                email: sesion.email.clone(),
                csrf: sesion.csrf_token.clone(),
                error: None,
                ok: None,
            }
            .render()
        },
        "mi_cuenta_form",
    )
}

#[derive(Debug, Deserialize)]
struct FormMiCuenta {
    #[serde(default)]
    csrf: String,
    actual: String,
    nueva: String,
    repetir: String,
}

async fn mi_cuenta_post(
    State(estado): State<EstadoServidor>,
    headers: HeaderMap,
    Sesion(sesion): Sesion,
    Form(formulario): Form<FormMiCuenta>,
) -> Response {
    let ip = ip_del_cliente(&headers);

    let pintar = |error: Option<String>, ok: Option<String>| {
        render(
            || {
                PlantillaMiCuenta {
                    version: VERSION.to_owned(),
                    email: sesion.email.clone(),
                    csrf: sesion.csrf_token.clone(),
                    error,
                    ok,
                }
                .render()
            },
            "mi_cuenta",
        )
    };

    if !sesiones::csrf_valido(&sesion, &formulario.csrf) {
        return (StatusCode::FORBIDDEN, "CSRF inválido").into_response();
    }

    if formulario.nueva != formulario.repetir {
        return pintar(None, Some("La password nueva y su repetición no coinciden.".to_owned()));
    }
    if formulario.nueva.chars().count() < 8 {
        return pintar(None, Some("La password nueva tiene que tener al menos 8 caracteres.".to_owned()));
    }

    // Verifica la actual contra la cuenta de ESTA sesión (sin tocar devices).
    match auth::autenticar_cuenta(&estado.pool, &sesion.email, &formulario.actual).await {
        Ok(Some(_)) => {}
        Ok(None) => return pintar(Some("La password actual no es correcta.".to_owned()), None),
        Err(error) => return error_interno("mi_cuenta_verificar", error),
    }

    let hash = match auth::hash_password(&formulario.nueva) {
        Ok(hash) => hash,
        Err(error) => return error_interno("mi_cuenta_hash", error),
    };

    let resultado = sqlx::query("UPDATE accounts SET password_hash = ? WHERE id = ?")
        .bind(hash)
        .bind(sesion.account_id)
        .execute(&estado.pool)
        .await;

    if let Err(error) = resultado {
        return error_interno("mi_cuenta_update", error);
    }

    // Las demás sesiones mueren (la actual sobrevive: tiene el token).
    if let Some(token) = cookie_de(&headers, COOKIE_SESION) {
        let _ = sesiones::revocar_otras_sesiones(&estado.pool, sesion.account_id, &token).await;
    }
    sesiones::auditar(
        &estado.pool,
        Some(sesion.account_id),
        "password_cambiada",
        None,
        ip.as_deref(),
    )
    .await;

    pintar(None, Some("Password actualizada. Las demás sesiones se cerraron.".to_owned()))
}

// ------------------------------------------------------------------ assets

async fn asset_css() -> Response {
    (
        [
            (CONTENT_TYPE, "text/css; charset=utf-8"),
            (CACHE_CONTROL, "public, max-age=86400"),
        ],
        ADMIN_CSS,
    )
        .into_response()
}

async fn asset_htmx() -> Response {
    (
        [
            (CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (CACHE_CONTROL, "public, max-age=86400"),
        ],
        HTMX_JS,
    )
        .into_response()
}

// `CAMPO_CSRF` vive en sesiones: es el nombre del campo oculto que las plantillas usan.
const _: &str = CAMPO_CSRF;

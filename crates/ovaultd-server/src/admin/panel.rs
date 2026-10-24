//! Páginas y handlers del panel de administración (ADR-0017).
//!
//! Server-rendered con askama (templates compilados dentro del binario) y htmx vendorizado
//! (`assets/htmx.min.js`, sin CDNs). El panel funciona igual sin JS: los formularios son
//! POST clásicos.
//!
//! Autorización: `/admin` exige sesión (cookie opaca). Cada POST exige además el token CSRF
//! de la sesión (tiempo constante). Las páginas autenticadas van con `cache-control: no-store`.
//! Los textos visibles salen de [`super::textos`] (i18n): acá no hay strings de UI sueltos.

use askama::Template;
use axum::extract::{FromRequestParts, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, REFERRER_POLICY, SET_COOKIE,
    USER_AGENT, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;

use super::hora;
use super::textos::{self, Textos};
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
        .merge(super::cuentas::rutas())
        .merge(super::vaults::rutas())
        .merge(super::devices::rutas())
        .merge(super::auditoria::rutas())
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

/// ¿Este request llegó por HTTPS? El `X-Forwarded-Proto` del borde se obedece SOLO con
/// `trust_proxy` activo: si no, no hay señal de TLS confiable (el server habla HTTP detrás
/// del borde) y la respuesta es `false`.
pub(crate) fn es_https(headers: &HeaderMap, confiar_en_proxy: bool) -> bool {
    if !confiar_en_proxy {
        return false;
    }

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
        ModoSecureCookies::Auto => es_https(headers, estado.config.trust_proxy),
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

/// IP del cliente ya resuelta por el middleware (peer TCP; `X-Real-IP`/`X-Forwarded-For`
/// SOLO con `trust_proxy` activo — la decisión y la lógica viven en `crate::peer`).
pub(crate) fn ip_del_cliente(headers: &HeaderMap) -> Option<String> {
    crate::peer::ip_de(headers)
}

fn user_agent_de(headers: &HeaderMap) -> Option<&str> {
    headers.get(USER_AGENT).and_then(|valor| valor.to_str().ok())
}

/// CSP estricta: la hoja y htmx salen del propio server (nunca de un CDN) y no hay
/// nada inline — ni scripts, ni hojas, ni estilos de atributo. Cualquier `style=""`
/// que vuelva a aparecer lo bloquea el browser y lo caza `tests/panel-audit.mjs`.
const CSP_PANEL: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; \
     font-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";

/// `permissions-policy` no está como constante en el crate `http`: la declaramos.
const CABECERA_PERMISSIONS_POLICY: axum::http::HeaderName =
    axum::http::HeaderName::from_static("permissions-policy");

/// Cabeceras de seguridad comunes a todo lo que sirve el panel (páginas y assets).
const CABECERAS_SEGURIDAD: [(axum::http::HeaderName, &str); 4] = [
    (X_CONTENT_TYPE_OPTIONS, "nosniff"),
    (REFERRER_POLICY, "no-referrer"),
    (
        CABECERA_PERMISSIONS_POLICY,
        "geolocation=(), camera=(), microphone=(), payment=()",
    ),
    (X_FRAME_OPTIONS, "DENY"),
];

/// Aplica las cabeceras de seguridad. `con_csp` solo para lo que es HTML: una hoja o
/// un script no ejecuta CSP, pero sí agradecen el `nosniff`.
fn con_seguridad(mut respuesta: Response, con_csp: bool) -> Response {
    let cabeceras = respuesta.headers_mut();
    if con_csp {
        cabeceras.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP_PANEL));
    }
    for (nombre, valor) in CABECERAS_SEGURIDAD {
        cabeceras.insert(nombre, HeaderValue::from_static(valor));
    }
    respuesta
}

/// Página autenticada: nada se cachea.
fn respuesta_html(html: String) -> Response {
    let mut respuesta = con_seguridad(Html(html).into_response(), true);
    respuesta
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    respuesta
}

pub(crate) fn error_interno(contexto: &str, error: impl std::fmt::Display) -> Response {
    tracing::error!(contexto, %error, "error en el panel");
    let mut respuesta = con_seguridad(
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Html("<h1>500</h1><p>Error interno. Mirá los logs del servidor.</p>".to_owned()),
        )
            .into_response(),
        true,
    );
    respuesta
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    respuesta
}

pub(crate) fn render<F>(renderizar: F, contexto: &str) -> Response
where
    F: FnOnce() -> std::result::Result<String, askama::Error>,
{
    match renderizar() {
        Ok(html) => respuesta_html(html),
        Err(error) => error_interno(contexto, error),
    }
}

/// «hace N min» a partir de segundos (la query ya hizo la resta en SQLite).
fn hace_texto(segundos: i64) -> String {
    match segundos {
        s if s < 90 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86400 => format!("{} h", s / 3600),
        s => format!("{} días", s / 86400),
    }
}

// ------------------------------------------------------------------ plantillas

#[derive(Template)]
#[template(path = "admin/login.html")]
struct PlantillaLogin {
    t: &'static Textos,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/dashboard.html")]
struct PlantillaDashboard {
    t: &'static Textos,
    seccion: &'static str,
    seccion_titulo: &'static str,
    version: &'static str,
    email: String,
    rol: String,
    csrf: String,
    // hero de salud
    uptime: String,
    db_bytes: String,
    fragmentos: i64,
    espacio: String,
    espacio_vaults: String,
    hace: String,
    ultimo_device: String,
    ultima_sync: Option<String>,
    // KPIs
    cuentas: i64,
    vaults: i64,
    notas: i64,
    revisiones: i64,
    devices: i64,
    lotes: i64,
    // secciones
    vaults_lista: Vec<VistaVault>,
    dispositivos: Vec<VistaDeviceMini>,
    actividad: Vec<VistaEventoMini>,
    zona: String,
}

/// Vista de un vault para las tablas: SOLO lo que el server puede ver (es ciego a los
/// nombres de nota y al contenido — eso es de los clientes).
struct VistaVault {
    id: i64,
    name: String,
    notas: i64,
    revisiones: i64,
    espacio: String,
    ultima_revision: Option<String>,
}

struct VistaDeviceMini {
    nombre: String,
    cuenta: String,
    ultima_actividad: Option<String>,
}

struct VistaEventoMini {
    hora: String,
    action: String,
    detail: String,
    ip: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/vaults.html")]
struct PlantillaVaults {
    t: &'static Textos,
    seccion: &'static str,
    seccion_titulo: &'static str,
    version: &'static str,
    email: String,
    rol: String,
    csrf: String,
    vaults: Vec<VistaVault>,
    zona: String,
}

#[derive(Template)]
#[template(path = "admin/mi_cuenta.html")]
struct PlantillaMiCuenta {
    t: &'static Textos,
    seccion: &'static str,
    seccion_titulo: &'static str,
    version: &'static str,
    email: String,
    rol: String,
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
                t: textos::t(),
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
    let t = textos::t();
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
            Some(t.login_freno),
            ip.as_deref(),
        )
        .await;

        return (
            StatusCode::TOO_MANY_REQUESTS,
            [
                (CACHE_CONTROL, "no-store"),
                (axum::http::header::RETRY_AFTER, "60"),
            ],
            Html(format!("<h1>{}</h1>", t.login_freno)),
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
                t,
                error: Some(t.login_error.to_owned()),
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

#[derive(Debug, sqlx::FromRow)]
struct FilaHero {
    ultima: String,
    segundos: i64,
    device: Option<String>,
}

async fn dashboard(State(estado): State<EstadoServidor>, Sesion(sesion): Sesion) -> Response {
    let t = textos::t();

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
        // Solo los activos (mismo criterio que /admin/devices): el KPI no debe contar
        // vínculos abandonados sin uso.
        "SELECT COUNT(*) FROM devices d
          WHERE d.revoked_at IS NULL
            AND (d.last_seen IS NOT NULL
                 OR EXISTS (SELECT 1 FROM batches b WHERE b.device_id = d.id))",
    )
    .await;
    let lotes = contar(&estado.pool, "SELECT COUNT(*) FROM batches").await;
    let fragmentos = contar(&estado.pool, "SELECT COUNT(*) FROM blobs").await;

    let bytes: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(size), 0) FROM blobs")
        .fetch_one(&estado.pool)
        .await
        .unwrap_or(0);
    let bytes_vaults: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(b.size), 0) FROM blobs b JOIN vaults v ON v.id = b.vault_id")
            .fetch_one(&estado.pool)
            .await
            .unwrap_or(0);
    let db_bytes: i64 = sqlx::query_scalar(
        "SELECT (SELECT page_count FROM pragma_page_count()) * (SELECT page_size FROM pragma_page_size())",
    )
    .fetch_one(&estado.pool)
    .await
    .unwrap_or(0);

    // Último lote GLOBAL: cuándo, hace cuánto (resta en SQLite) y desde qué dispositivo.
    let hero = sqlx::query_as::<_, FilaHero>(
        "SELECT b.ts AS ultima,
                CAST((julianday('now') - julianday(b.ts)) * 86400 AS INTEGER) AS segundos,
                d.name AS device
         FROM batches b
         LEFT JOIN devices d ON d.id = b.device_id
         ORDER BY b.ts DESC
         LIMIT 1",
    )
    .fetch_optional(&estado.pool)
    .await
    .ok()
    .flatten();

    let desplazamiento = estado.config.tz_offset_minutos;
    let zona = hora::etiqueta_zona(desplazamiento);
    let (hace, ultimo_device, ultima_sync) = match hero {
        Some(fila) => (
            hace_texto(fila.segundos.max(0)),
            fila.device.unwrap_or_else(|| t.sin_datos.to_owned()),
            Some(hora::hora_local(&fila.ultima, desplazamiento)),
        ),
        None => (t.salud_hace_nada.to_owned(), t.sin_datos.to_owned(), None),
    };

    let vaults_lista = sqlx::query_as::<_, FilaVault>(
        "SELECT v.id, v.name,
                (SELECT COUNT(*) FROM files f WHERE f.vault_id = v.id AND f.deleted = 0) AS notas,
                (SELECT COUNT(*) FROM revisions r WHERE r.vault_id = v.id) AS revisiones,
                (SELECT COALESCE(SUM(b.size), 0) FROM blobs b WHERE b.vault_id = v.id) AS bytes,
                (SELECT MAX(r.ts) FROM revisions r WHERE r.vault_id = v.id) AS ultima_revision
         FROM vaults v
         ORDER BY v.id
         LIMIT 6",
    )
    .fetch_all(&estado.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|fila| vista_de_fila(fila, desplazamiento))
    .collect();

    let dispositivos = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT d.name, a.email,
                NULLIF(max(COALESCE((SELECT MAX(b.ts) FROM batches b WHERE b.device_id = d.id), ''), COALESCE(d.last_seen, '')), '')
         FROM devices d
         JOIN accounts a ON a.id = d.account_id
         WHERE d.revoked_at IS NULL
         ORDER BY d.id DESC
         LIMIT 5",
    )
    .fetch_all(&estado.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(nombre, cuenta, ultima_actividad)| VistaDeviceMini {
        nombre,
        cuenta,
        ultima_actividad: ultima_actividad
            .as_deref()
            .map(|ts| hora::hora_local(ts, desplazamiento)),
    })
    .collect();

    let actividad = sqlx::query_as::<_, (String, String, String, Option<String>)>(
        "SELECT ts AS hora, action, COALESCE(detail, '') AS detail, ip
         FROM audit_log
         ORDER BY id DESC
         LIMIT 5",
    )
    .fetch_all(&estado.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(hora, action, detail, ip)| {
        let local = hora::hora_local(&hora, desplazamiento);
        VistaEventoMini {
            // HH:MM del huso configurado (el `ts` completo vive en /admin/auditoria).
            hora: local.get(11..16).unwrap_or(&local).to_owned(),
            action,
            detail,
            ip,
        }
    })
    .collect();

    render(
        || {
            PlantillaDashboard {
                t,
                seccion: "panel",
                seccion_titulo: t.nav_panel,
                version: VERSION,
                email: sesion.email.clone(),
                rol: sesion.role.clone(),
                csrf: sesion.csrf_token.clone(),
                uptime: uptime_texto(crate::routes::uptime_segundos()),
                db_bytes: formato_bytes(db_bytes),
                fragmentos,
                espacio: formato_bytes(bytes),
                espacio_vaults: formato_bytes(bytes_vaults),
                hace,
                ultimo_device,
                ultima_sync,
                cuentas,
                vaults,
                notas,
                revisiones,
                devices,
                lotes,
                vaults_lista,
                dispositivos,
                actividad,
                zona,
            }
            .render()
        },
        "dashboard",
    )
}

/// «arriba 13 min» / «arriba 2 h» / «arriba 3 días» desde el arranque del proceso.
fn uptime_texto(segundos: u64) -> String {
    match segundos {
        s if s < 90 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86400 => format!("{} h", s / 3600),
        s => format!("{} días", s / 86400),
    }
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

fn vista_de_fila(fila: FilaVault, offset_minutos: i32) -> VistaVault {
    VistaVault {
        id: fila.id,
        name: fila.name,
        notas: fila.notas,
        revisiones: fila.revisiones,
        espacio: formato_bytes(fila.bytes),
        ultima_revision: fila
            .ultima_revision
            .as_deref()
            .map(|ts| hora::hora_local(ts, offset_minutos)),
    }
}

/// Estadísticas por vault: notas vivas (`files` sin borrar), revisiones (histórico),
/// espacio de los blobs cifrados y última actividad. El server NO tiene los nombres.
async fn vaults(State(estado): State<EstadoServidor>, Sesion(sesion): Sesion) -> Response {
    let t = textos::t();

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

    let desplazamiento = estado.config.tz_offset_minutos;
    let zona = hora::etiqueta_zona(desplazamiento);
    let vista = filas
        .into_iter()
        .map(|fila| vista_de_fila(fila, desplazamiento))
        .collect();

    render(
        || {
            PlantillaVaults {
                t,
                seccion: "vaults",
                seccion_titulo: t.nav_vaults,
                version: VERSION,
                email: sesion.email.clone(),
                rol: sesion.role.clone(),
                csrf: sesion.csrf_token.clone(),
                vaults: vista,
                zona,
            }
            .render()
        },
        "vaults",
    )
}

/// Tamaño legible para humanos en formato español (coma decimal).
pub(crate) fn formato_bytes(bytes: i64) -> String {
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
        format!("{valor:.1} {}", UNIDADES[unidad]).replace('.', ",")
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
    let t = textos::t();
    render(
        || {
            PlantillaMiCuenta {
                t,
                seccion: "panel",
                seccion_titulo: t.mi_cuenta_titulo,
                version: VERSION,
                email: sesion.email.clone(),
                rol: sesion.role.clone(),
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
    let t = textos::t();
    let ip = ip_del_cliente(&headers);

    let pintar = |error: Option<String>, ok: Option<String>| {
        render(
            || {
                PlantillaMiCuenta {
                    t,
                    seccion: "panel",
                    seccion_titulo: t.mi_cuenta_titulo,
                    version: VERSION,
                    email: sesion.email.clone(),
                    rol: sesion.role.clone(),
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
        return pintar(Some(t.msg_passwords_no_coinciden.to_owned()), None);
    }
    if crate::auth::password_debil(&formulario.nueva) {
        return pintar(Some(t.msg_password_corta.to_owned()), None);
    }

    // Verifica la actual contra la cuenta de ESTA sesión (sin tocar devices).
    match auth::autenticar_cuenta(&estado.pool, &sesion.email, &formulario.actual).await {
        Ok(Some(_)) => {}
        Ok(None) => return pintar(Some(t.mi_cuenta_actual_incorrecta.to_owned()), None),
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

    pintar(None, Some(t.mi_cuenta_ok.to_owned()))
}

// ------------------------------------------------------------------ assets

/// ETag del asset embebido: con `no-cache` el browser revalida en cada carga y recibe
/// 304 cuando no cambió — siempre fresco, sin el clásico «css viejo con html nuevo».
fn etag_de(contenido: &'static str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(contenido.as_bytes());
    let corto: String = hash[..8].iter().map(|byte| format!("{byte:02x}")).collect();
    format!("\"{corto}\"")
}

fn responder_asset(contenido: &'static str, tipo: &'static str, headers: &HeaderMap) -> Response {
    let etag = etag_de(contenido);

    if headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|valor| valor.to_str().ok())
        == Some(etag.as_str())
    {
        let mut respuesta = StatusCode::NOT_MODIFIED.into_response();
        if let Ok(valor) = HeaderValue::from_str(&etag) {
            respuesta.headers_mut().insert(axum::http::header::ETAG, valor);
        }
        return respuesta;
    }

    let mut respuesta = (StatusCode::OK, contenido).into_response();
    let cabeceras = respuesta.headers_mut();
    cabeceras.insert(CONTENT_TYPE, HeaderValue::from_static(tipo));
    cabeceras.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Ok(valor) = HeaderValue::from_str(&etag) {
        cabeceras.insert(axum::http::header::ETAG, valor);
    }
    con_seguridad(respuesta, false)
}

async fn asset_css(headers: HeaderMap) -> Response {
    responder_asset(ADMIN_CSS, "text/css; charset=utf-8", &headers)
}

async fn asset_htmx(headers: HeaderMap) -> Response {
    responder_asset(HTMX_JS, "text/javascript; charset=utf-8", &headers)
}

// `CAMPO_CSRF` vive en sesiones: es el nombre del campo oculto que las plantillas usan.
const _: &str = CAMPO_CSRF;

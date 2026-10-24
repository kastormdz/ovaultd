//! W2: devices — vista global con revocación.
//!
//! Un device agrupa los tokens de API/WS de un cliente (el plugin, el CLI, la web de
//! setup no: esa usa sesión). Revocar es DEFINITIVO: `auth::revocar_device` marca el
//! device y todos sus tokens; el cliente vuelve a loguear y obtiene uno nuevo.

use askama::Template;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use serde::Deserialize;

use super::{flash, panel, sesiones, SesionAdmin};
use super::textos::{self, Textos};
use crate::auth;
use crate::routes::EstadoServidor;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const POR_PAGINA: i64 = 25;

pub fn rutas() -> Router<EstadoServidor> {
    Router::new()
        .route("/admin/devices", get(lista))
        .route("/admin/devices/revocar", post(revocar))
}

#[derive(Template)]
#[template(path = "admin/devices.html")]
struct PlantillaDevices {
    t: &'static Textos,
    seccion: &'static str,
    seccion_titulo: &'static str,
    version: &'static str,
    email: String,
    rol: String,
    csrf: String,
    devices: Vec<VistaDevice>,
    pagina: i64,
    hay_mas: bool,
    ver_todos: bool,
    inactivos: i64,
    error: Option<String>,
    ok: Option<String>,
}

struct VistaDevice {
    id: i64,
    cuenta: String,
    nombre: String,
    plataforma: String,
    creado: String,
    ultima_actividad: Option<String>,
    revocado: bool,
    tokens_vivos: i64,
}

#[derive(Debug, Deserialize)]
struct ParamsLista {
    #[serde(default)]
    pagina: Option<i64>,
    /// `?todos=1` incluye los devices inactivos (para auditarlos o revocarlos).
    #[serde(default)]
    todos: Option<u8>,
    #[serde(default)]
    ok: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct FilaDevice {
    id: i64,
    cuenta: String,
    nombre: String,
    plataforma: Option<String>,
    creado: String,
    ultima_actividad: Option<String>,
    revocado: Option<String>,
    tokens_vivos: i64,
}

async fn lista(
    State(estado): State<EstadoServidor>,
    SesionAdmin(sesion): SesionAdmin,
    Query(params): Query<ParamsLista>,
) -> Response {
    pintar(
        &estado,
        &sesion,
        params.pagina.unwrap_or(1).max(1),
        params.todos.unwrap_or(0) != 0,
        params.ok,
        params.error,
    )
    .await
}

async fn pintar(
    estado: &EstadoServidor,
    sesion: &sesiones::SesionActiva,
    pagina: i64,
    todos: bool,
    ok: Option<String>,
    error: Option<String>,
) -> Response {
    let offset = (pagina - 1) * POR_PAGINA;

    // "Activo" = el device dio señales de vida: el latido de `autenticar_token`
    // (`last_seen`) o al menos un lote commiteado. Un vínculo abandonado (dos creaciones
    // y nunca usado) queda fuera de la lista por defecto; `?todos=1` lo muestra para
    // poder auditarlo o revocarlo. Dos literales: sqlx 0.9 exige SqlSafeStr.
    let por_defecto = "SELECT d.id, a.email AS cuenta, d.name AS nombre, d.platform AS plataforma,
                d.created_at AS creado,
                max(d.last_seen,
                         (SELECT MAX(b.ts) FROM batches b WHERE b.device_id = d.id)) AS ultima_actividad,
                d.revoked_at AS revocado,
                (SELECT COUNT(*) FROM device_tokens t
                  WHERE t.device_id = d.id AND t.revoked_at IS NULL
                    AND t.expires_at > datetime('now')) AS tokens_vivos
         FROM devices d
         JOIN accounts a ON a.id = d.account_id
         WHERE d.revoked_at IS NULL
           AND (d.last_seen IS NOT NULL
                OR EXISTS (SELECT 1 FROM batches b WHERE b.device_id = d.id))
         ORDER BY d.id DESC
         LIMIT ? OFFSET ?";
    let con_todos = "SELECT d.id, a.email AS cuenta, d.name AS nombre, d.platform AS plataforma,
                d.created_at AS creado,
                max(d.last_seen,
                         (SELECT MAX(b.ts) FROM batches b WHERE b.device_id = d.id)) AS ultima_actividad,
                d.revoked_at AS revocado,
                (SELECT COUNT(*) FROM device_tokens t
                  WHERE t.device_id = d.id AND t.revoked_at IS NULL
                    AND t.expires_at > datetime('now')) AS tokens_vivos
         FROM devices d
         JOIN accounts a ON a.id = d.account_id
         ORDER BY d.id DESC
         LIMIT ? OFFSET ?";

    let filas = match sqlx::query_as::<_, FilaDevice>(if todos { con_todos } else { por_defecto })
        .bind(POR_PAGINA + 1)
        .bind(offset)
        .fetch_all(&estado.pool)
        .await
    {
        Ok(filas) => filas,
        Err(error) => return panel::error_interno("devices_lista", error),
    };

    let inactivos = if todos {
        0
    } else {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM devices d
             WHERE d.revoked_at IS NULL
               AND NOT (d.last_seen IS NOT NULL
                        OR EXISTS (SELECT 1 FROM batches b WHERE b.device_id = d.id))",
        )
        .fetch_one(&estado.pool)
        .await
        .unwrap_or(0)
    };

    let hay_mas = filas.len() as i64 > POR_PAGINA;
    let devices = filas
        .into_iter()
        .take(POR_PAGINA as usize)
        .map(|fila| VistaDevice {
            id: fila.id,
            cuenta: fila.cuenta,
            nombre: fila.nombre,
            plataforma: fila.plataforma.unwrap_or_else(|| "—".to_owned()),
            creado: fila.creado,
            ultima_actividad: fila.ultima_actividad,
            revocado: fila.revocado.is_some(),
            tokens_vivos: fila.tokens_vivos,
        })
        .collect();

    panel::render(
        || {
            PlantillaDevices {
                t: textos::t(),
                seccion: "dispositivos",
                seccion_titulo: textos::t().nav_dispositivos,
                version: VERSION,
                email: sesion.email.clone(),
                rol: sesion.role.clone(),
                csrf: sesion.csrf_token.clone(),
                devices,
                pagina,
                hay_mas,
                ver_todos: todos,
                inactivos,
                error,
                ok,
            }
            .render()
        },
        "devices",
    )
}

#[derive(Debug, Deserialize)]
struct FormRevocar {
    #[serde(default)]
    csrf: String,
    id: i64,
}

async fn revocar(
    State(estado): State<EstadoServidor>,
    headers: HeaderMap,
    SesionAdmin(sesion): SesionAdmin,
    Form(formulario): Form<FormRevocar>,
) -> Response {
    let ip = panel::ip_del_cliente(&headers);

    if !sesiones::csrf_valido(&sesion, &formulario.csrf) {
        return (StatusCode::FORBIDDEN, "CSRF inválido").into_response();
    }

    let device = match sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT d.name, a.email, d.revoked_at
         FROM devices d
         JOIN accounts a ON a.id = d.account_id
         WHERE d.id = ?",
    )
    .bind(formulario.id)
    .fetch_optional(&estado.pool)
    .await
    {
        Ok(Some(fila)) => fila,
        Ok(None) => {
            return Redirect::to(&format!(
                "/admin/devices?error={}",
                flash(textos::t().msg_device_no_existe)
            ))
            .into_response();
        }
        Err(error) => return panel::error_interno("devices_revocar_target", error),
    };

    if device.2.is_some() {
        return Redirect::to(&format!(
            "/admin/devices?error={}",
            flash(textos::t().msg_device_ya_revocado)
        ))
        .into_response();
    }

    if let Err(error) = auth::revocar_device(&estado.pool, formulario.id).await {
        return panel::error_interno("devices_revocar", error);
    }

    sesiones::auditar(
        &estado.pool,
        Some(sesion.account_id),
        "device_revocado",
        Some(&format!("#{} «{}» ({})", formulario.id, device.0, device.1)),
        ip.as_deref(),
    )
    .await;

    Redirect::to(&format!(
        "/admin/devices?ok={}",
        flash(textos::t().msg_device_revocado)
    ))
    .into_response()
}

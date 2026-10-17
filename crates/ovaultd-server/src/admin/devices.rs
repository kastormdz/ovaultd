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
    version: String,
    email: String,
    csrf: String,
    devices: Vec<VistaDevice>,
    pagina: i64,
    hay_mas: bool,
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
        params.ok,
        params.error,
    )
    .await
}

async fn pintar(
    estado: &EstadoServidor,
    sesion: &sesiones::SesionActiva,
    pagina: i64,
    ok: Option<String>,
    error: Option<String>,
) -> Response {
    let offset = (pagina - 1) * POR_PAGINA;

    let filas = match sqlx::query_as::<_, FilaDevice>(
        "SELECT d.id, a.email AS cuenta, d.name AS nombre, d.platform AS plataforma,
                d.created_at AS creado,
                COALESCE(d.last_seen,
                         (SELECT MAX(b.ts) FROM batches b WHERE b.device_id = d.id)) AS ultima_actividad,
                d.revoked_at AS revocado,
                (SELECT COUNT(*) FROM device_tokens t
                  WHERE t.device_id = d.id AND t.revoked_at IS NULL
                    AND t.expires_at > datetime('now')) AS tokens_vivos
         FROM devices d
         JOIN accounts a ON a.id = d.account_id
         ORDER BY d.id DESC
         LIMIT ? OFFSET ?",
    )
    .bind(POR_PAGINA + 1)
    .bind(offset)
    .fetch_all(&estado.pool)
    .await
    {
        Ok(filas) => filas,
        Err(error) => return panel::error_interno("devices_lista", error),
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
                version: VERSION.to_owned(),
                email: sesion.email.clone(),
                csrf: sesion.csrf_token.clone(),
                devices,
                pagina,
                hay_mas,
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
                flash("Ese device no existe.")
            ))
            .into_response();
        }
        Err(error) => return panel::error_interno("devices_revocar_target", error),
    };

    if device.2.is_some() {
        return Redirect::to(&format!(
            "/admin/devices?error={}",
            flash("Ese device ya estaba revocado.")
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

    Redirect::to("/admin/devices?ok=Device+revocado.").into_response()
}

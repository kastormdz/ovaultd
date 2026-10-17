//! W2: vaults — detalle y borrado desde el panel.
//!
//! El server es CIEGO: acá se muestra solo lo que puede ver (conteos, bytes, revisiones,
//! miembros, batches). Ni nombres de nota ni contenido, jamás.
//! Borrar un vault es la MISMA operación que la API (`purgar_vault`): cascada + barrido
//! del CAS + auditoría; la confirmación es por NOMBRE EXACTO (un click no basta).

use askama::Template;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Form, Router,
};
use serde::Deserialize;

use super::{panel, sesiones, SesionAdmin};
use super::textos::{self, Textos};
use crate::routes::{purgar_vault, EstadoServidor};

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn rutas() -> Router<EstadoServidor> {
    Router::new()
        .route("/admin/vaults/detalle", get(detalle))
        .route(
            "/admin/vaults/borrar",
            get(borrar_form).post(borrar_post),
        )
}

// ------------------------------------------------------------------ plantillas

#[derive(Template)]
#[template(path = "admin/vault_detalle.html")]
struct PlantillaDetalle {
    t: &'static Textos,
    seccion: &'static str,
    seccion_titulo: &'static str,
    version: &'static str,
    email: String,
    rol: String,
    csrf: String,
    vault_id: i64,
    nombre: String,
    dueno: String,
    creado: String,
    epoch: i64,
    head_rev: i64,
    enc_version: i64,
    notas: i64,
    revisiones: i64,
    batch_count: i64,
    blob_count: i64,
    bytes: String,
    cuota: String,
    max_file: String,
    miembros: Vec<VistaMiembro>,
    ultimos: Vec<VistaBatch>,
}

struct VistaMiembro {
    email: String,
    role: String,
    desde: String,
}

struct VistaBatch {
    rev: i64,
    device: String,
    ops: i64,
    ts: String,
}

#[derive(Template)]
#[template(path = "admin/vault_borrar.html")]
struct PlantillaBorrar {
    t: &'static Textos,
    seccion: &'static str,
    seccion_titulo: &'static str,
    version: &'static str,
    email: String,
    rol: String,
    csrf: String,
    vault_id: i64,
    nombre: String,
    dueno: String,
    notas: i64,
    revisiones: i64,
    bytes: String,
    error: Option<String>,
}

// ------------------------------------------------------------------ detalle

#[derive(Debug, Deserialize)]
struct ParamsDetalle {
    #[serde(default)]
    id: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
struct FilaDetalle {
    id: i64,
    name: String,
    dueno: String,
    created_at: String,
    epoch: i64,
    head_rev: i64,
    enc_version: i64,
    notas: i64,
    revisiones: i64,
    batches: i64,
    blob_count: i64,
    bytes: i64,
    quota_bytes: Option<i64>,
    max_file_bytes: Option<i64>,
}

async fn detalle(
    State(estado): State<EstadoServidor>,
    super::SesionAdmin(sesion): SesionAdmin,
    Query(params): Query<ParamsDetalle>,
) -> Response {
    let Some(id) = params.id else {
        return Redirect::to("/admin/vaults").into_response();
    };

    let fila = sqlx::query_as::<_, FilaDetalle>(
        "SELECT v.id, v.name, a.email AS dueno, v.created_at, v.epoch, v.head_rev, v.enc_version,
                (SELECT COUNT(*) FROM files f WHERE f.vault_id = v.id AND f.deleted = 0) AS notas,
                (SELECT COUNT(*) FROM revisions r WHERE r.vault_id = v.id) AS revisiones,
                (SELECT COUNT(*) FROM batches b WHERE b.vault_id = v.id) AS batches,
                (SELECT COUNT(*) FROM blobs bl WHERE bl.vault_id = v.id) AS blob_count,
                (SELECT COALESCE(SUM(bl.size), 0) FROM blobs bl WHERE bl.vault_id = v.id) AS bytes,
                v.quota_bytes, v.max_file_bytes
         FROM vaults v
         JOIN accounts a ON a.id = v.account_id
         WHERE v.id = ?",
    )
    .bind(id)
    .fetch_optional(&estado.pool)
    .await;

    let fila = match fila {
        Ok(Some(fila)) => fila,
        Ok(None) => return Redirect::to("/admin/vaults").into_response(),
        Err(error) => return panel::error_interno("vault_detalle", error),
    };

    let miembros = sqlx::query_as::<_, (String, String, String)>(
        "SELECT a.email, m.role, m.created_at
         FROM vault_members m
         JOIN accounts a ON a.id = m.account_id
         WHERE m.vault_id = ?
         ORDER BY m.created_at",
    )
    .bind(id)
    .fetch_all(&estado.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(email, role, desde)| VistaMiembro { email, role, desde })
    .collect();

    let ultimos = sqlx::query_as::<_, (i64, Option<String>, i64, String)>(
        "SELECT b.rev, d.name, b.op_count, b.ts
         FROM batches b
         LEFT JOIN devices d ON d.id = b.device_id
         WHERE b.vault_id = ?
         ORDER BY b.rev DESC
         LIMIT 10",
    )
    .bind(id)
    .fetch_all(&estado.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(rev, device, ops, ts)| VistaBatch {
        rev,
        device: device.unwrap_or_else(|| "—".to_owned()),
        ops,
        ts,
    })
    .collect();

    panel::render(
        || {
            PlantillaDetalle {
                t: textos::t(),
                seccion: "vaults",
                seccion_titulo: textos::t().nav_vaults,
                version: VERSION,
                email: sesion.email.clone(),
                rol: sesion.role.clone(),
                csrf: sesion.csrf_token.clone(),
                vault_id: fila.id,
                nombre: fila.name,
                dueno: fila.dueno,
                creado: fila.created_at,
                epoch: fila.epoch,
                head_rev: fila.head_rev,
                enc_version: fila.enc_version,
                notas: fila.notas,
                revisiones: fila.revisiones,
                batch_count: fila.batches,
                blob_count: fila.blob_count,
                bytes: super::panel::formato_bytes(fila.bytes),
                cuota: fila
                    .quota_bytes
                    .map(super::panel::formato_bytes)
                    .unwrap_or_else(|| "ilimitada".to_owned()),
                max_file: fila
                    .max_file_bytes
                    .map(super::panel::formato_bytes)
                    .unwrap_or_else(|| "del protocolo (32 MiB)".to_owned()),
                miembros,
                ultimos,
            }
            .render()
        },
        "vault_detalle",
    )
}

// ------------------------------------------------------------------ borrado

#[derive(Debug, Deserialize)]
struct ParamsBorrar {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    error: Option<String>,
}

async fn borrar_form(
    State(estado): State<EstadoServidor>,
    SesionAdmin(sesion): SesionAdmin,
    Query(params): Query<ParamsBorrar>,
) -> Response {
    let Some(id) = params.id else {
        return Redirect::to("/admin/vaults").into_response();
    };

    let fila = sqlx::query_as::<_, (String, String, i64, i64, i64)>(
        "SELECT v.name, a.email,
                (SELECT COUNT(*) FROM files f WHERE f.vault_id = v.id AND f.deleted = 0),
                (SELECT COUNT(*) FROM revisions r WHERE r.vault_id = v.id),
                (SELECT COALESCE(SUM(bl.size), 0) FROM blobs bl WHERE bl.vault_id = v.id)
         FROM vaults v
         JOIN accounts a ON a.id = v.account_id
         WHERE v.id = ?",
    )
    .bind(id)
    .fetch_optional(&estado.pool)
    .await;

    let (nombre, dueno, notas, revisiones, bytes) = match fila {
        Ok(Some(fila)) => fila,
        Ok(None) => return Redirect::to("/admin/vaults").into_response(),
        Err(error) => return panel::error_interno("vault_borrar_form", error),
    };

    panel::render(
        || {
            PlantillaBorrar {
                t: textos::t(),
                seccion: "vaults",
                seccion_titulo: textos::t().nav_vaults,
                version: VERSION,
                email: sesion.email.clone(),
                rol: sesion.role.clone(),
                csrf: sesion.csrf_token.clone(),
                vault_id: id,
                nombre,
                dueno,
                notas,
                revisiones,
                bytes: panel::formato_bytes(bytes),
                error: params.error,
            }
            .render()
        },
        "vault_borrar",
    )
}

#[derive(Debug, Deserialize)]
struct FormBorrar {
    #[serde(default)]
    csrf: String,
    id: i64,
    confirm: String,
}

async fn borrar_post(
    State(estado): State<EstadoServidor>,
    headers: HeaderMap,
    SesionAdmin(sesion): SesionAdmin,
    Form(formulario): Form<FormBorrar>,
) -> Response {
    let ip = panel::ip_del_cliente(&headers);

    let volver_error = |mensaje: &str| {
        Redirect::to(&format!(
            "/admin/vaults/borrar?id={}&error={}",
            formulario.id,
            super::flash(mensaje)
        ))
        .into_response()
    };

    if !sesiones::csrf_valido(&sesion, &formulario.csrf) {
        return (StatusCode::FORBIDDEN, "CSRF inválido").into_response();
    }

    let fila = sqlx::query_as::<_, (String, i64, String)>(
        "SELECT v.name, v.account_id, a.email
         FROM vaults v
         JOIN accounts a ON a.id = v.account_id
         WHERE v.id = ?",
    )
    .bind(formulario.id)
    .fetch_optional(&estado.pool)
    .await;

    let (nombre, _dueno_id, dueno_email) = match fila {
        Ok(Some(fila)) => fila,
        Ok(None) => return Redirect::to("/admin/vaults").into_response(),
        Err(error) => return panel::error_interno("vault_borrar_target", error),
    };

    if formulario.confirm.trim() != nombre {
        return volver_error(textos::t().msg_vault_nombre_no_coincide);
    }

    if let Err(error) = purgar_vault(&estado.pool, &estado.cas, formulario.id).await {
        return panel::error_interno("vault_borrar_purgar", error);
    }

    sesiones::auditar(
        &estado.pool,
        Some(sesion.account_id),
        "vault_delete",
        Some(&format!(
            "#{} «{nombre}» (de {dueno_email}, desde el panel)",
            formulario.id
        )),
        ip.as_deref(),
    )
    .await;

    Redirect::to(&format!(
        "/admin/vaults?ok={}",
        crate::admin::flash(textos::t().msg_vault_borrado)
    ))
    .into_response()
}

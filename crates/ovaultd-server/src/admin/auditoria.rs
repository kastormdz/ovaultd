//! W2: auditoría — la bitácora del server, paginada y filtrable.
//!
//! Solo lectura. Cada acción sensible (login, cuentas, vaults, devices, ajustes) deja
//! su fila en `audit_log`; acá se muestra tal cual, sin interpretación.

use askama::Template;
use axum::{
    extract::{Query, State},
    response::Response,
};

use serde::Deserialize;

use super::{panel, SesionAdmin};
use super::textos::{self, Textos};
use crate::routes::EstadoServidor;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const POR_PAGINA: i64 = 50;

pub fn rutas() -> axum::Router<EstadoServidor> {
    axum::Router::new().route("/admin/auditoria", axum::routing::get(lista))
}

#[derive(Template)]
#[template(path = "admin/auditoria.html")]
struct PlantillaAuditoria {
    t: &'static Textos,
    seccion: &'static str,
    seccion_titulo: &'static str,
    version: &'static str,
    email: String,
    rol: String,
    csrf: String,
    eventos: Vec<VistaEvento>,
    pagina: i64,
    hay_mas: bool,
    /// Filtro aplicado (por acción o detalle), si hay.
    q: String,
    total: i64,
}

struct VistaEvento {
    ts: String,
    actor: String,
    action: String,
    detail: Option<String>,
    ip: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ParamsLista {
    #[serde(default)]
    pagina: Option<i64>,
    #[serde(default)]
    q: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct FilaEvento {
    ts: String,
    actor: Option<String>,
    action: String,
    detail: Option<String>,
    ip: Option<String>,
}

async fn lista(
    State(estado): State<EstadoServidor>,
    SesionAdmin(sesion): SesionAdmin,
    Query(params): Query<ParamsLista>,
) -> Response {
    let pagina = params.pagina.unwrap_or(1).max(1);
    let offset = (pagina - 1) * POR_PAGINA;

    // Filtro chico y honesto: la MISMA subcadena contra action y detail.
    let q = params
        .q
        .unwrap_or_default()
        .trim()
        .chars()
        .take(80)
        .collect::<String>();
    let patron = format!("%{q}%");

    let (total, filas) = if q.is_empty() {
        let total = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(&estado.pool)
            .await
            .unwrap_or(0);
        let filas = sqlx::query_as::<_, FilaEvento>(
            "SELECT l.ts, a.email AS actor, l.action, l.detail, l.ip
             FROM audit_log l
             LEFT JOIN accounts a ON a.id = l.actor_account_id
             ORDER BY l.id DESC
             LIMIT ? OFFSET ?",
        )
        .bind(POR_PAGINA + 1)
        .bind(offset)
        .fetch_all(&estado.pool)
        .await;
        (total, filas)
    } else {
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM audit_log WHERE action LIKE ? OR detail LIKE ?",
        )
        .bind(&patron)
        .bind(&patron)
        .fetch_one(&estado.pool)
        .await
        .unwrap_or(0);
        let filas = sqlx::query_as::<_, FilaEvento>(
            "SELECT l.ts, a.email AS actor, l.action, l.detail, l.ip
             FROM audit_log l
             LEFT JOIN accounts a ON a.id = l.actor_account_id
             WHERE l.action LIKE ? OR l.detail LIKE ?
             ORDER BY l.id DESC
             LIMIT ? OFFSET ?",
        )
        .bind(&patron)
        .bind(&patron)
        .bind(POR_PAGINA + 1)
        .bind(offset)
        .fetch_all(&estado.pool)
        .await;
        (total, filas)
    };

    let filas = match filas {
        Ok(filas) => filas,
        Err(error) => return panel::error_interno("auditoria_lista", error),
    };

    let hay_mas = filas.len() as i64 > POR_PAGINA;
    let eventos = filas
        .into_iter()
        .take(POR_PAGINA as usize)
        .map(|fila| VistaEvento {
            ts: fila.ts,
            actor: fila.actor.unwrap_or_else(|| "—".to_owned()),
            action: fila.action,
            detail: fila.detail,
            ip: fila.ip,
        })
        .collect();

    panel::render(
        || {
            PlantillaAuditoria {
                t: textos::t(),
                seccion: "auditoria",
                seccion_titulo: textos::t().nav_auditoria,
                version: VERSION,
                email: sesion.email.clone(),
                rol: sesion.role.clone(),
                csrf: sesion.csrf_token.clone(),
                eventos,
                pagina,
                hay_mas,
                q,
                total,
            }
            .render()
        },
        "auditoria",
    )
}

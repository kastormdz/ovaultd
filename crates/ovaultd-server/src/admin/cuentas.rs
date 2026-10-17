//! W1: administración de cuentas desde el panel (ADR-0017).
//!
//! Reglas de oro:
//!  - Toda página de administración exige rol `admin` (extractor [`SesionAdmin`]); sin rol,
//!    página 403 (sin redirect: la sesión es válida, el permiso no).
//!  - Todo POST con CSRF (menos `/reset`, donde el token de un uso en la URL ES la prueba;
//!    no hay cookie de sesión que un CSRF pudiera abusar).
//!  - Jamás se borra, deshabilita ni degrada al ÚLTIMO admin activo; a uno mismo no se
//!    puede borrar ni deshabilitar.
//!  - Todo queda auditado.

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
use crate::routes::{purgar_vault, EstadoServidor};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const POR_PAGINA: i64 = 25;
const RESET_VIGENCIA_HORAS: i64 = 24;

pub fn rutas() -> Router<EstadoServidor> {
    Router::new()
        .route("/admin/cuentas", get(cuentas_lista))
        .route("/admin/cuentas/crear", post(cuentas_crear))
        .route("/admin/cuentas/accion", post(cuentas_accion))
        .route(
            "/admin/cuentas/borrar",
            get(cuenta_borrar_form).post(cuenta_borrar_post),
        )
        .route("/admin/ajustes", get(ajustes_form).post(ajustes_post))
        .route("/register", get(register_form).post(register_post))
        .route("/reset", get(reset_form).post(reset_post))
}

// ------------------------------------------------------------------ plantillas

#[derive(Template)]
#[template(path = "admin/cuentas.html")]
struct PlantillaCuentas {
    t: &'static Textos,
    seccion: &'static str,
    seccion_titulo: &'static str,
    version: &'static str,
    email: String,
    rol: String,
    csrf: String,
    cuentas: Vec<VistaCuenta>,
    pagina: i64,
    hay_mas: bool,
    error: Option<String>,
    ok: Option<String>,
    /// Link de reset recién generado (se muestra UNA vez en la respuesta).
    reset_link: Option<String>,
}

struct VistaCuenta {
    id: i64,
    email: String,
    role: String,
    creada: String,
    deshabilitada: bool,
    es_yo: bool,
    vaults: i64,
    ultima: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/cuenta_borrar.html")]
struct PlantillaCuentaBorrar {
    t: &'static Textos,
    seccion: &'static str,
    seccion_titulo: &'static str,
    version: &'static str,
    email: String,
    rol: String,
    csrf: String,
    cuenta_id: i64,
    cuenta_email: String,
    cuenta_rol: String,
    vaults: Vec<VistaVaultDeCuenta>,
    error: Option<String>,
}

struct VistaVaultDeCuenta {
    id: i64,
    name: String,
}

#[derive(Template)]
#[template(path = "admin/ajustes.html")]
struct PlantillaAjustes {
    t: &'static Textos,
    seccion: &'static str,
    seccion_titulo: &'static str,
    version: &'static str,
    email: String,
    rol: String,
    csrf: String,
    registro_abierto: bool,
    ok: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/register.html")]
struct PlantillaRegister {
    t: &'static Textos,
    abierto: bool,
    error: Option<String>,
    hecho: bool,
}

#[derive(Template)]
#[template(path = "admin/reset.html")]
struct PlantillaReset {
    t: &'static Textos,
    /// Token válido que viaja en el form (la autorización).
    token: String,
    email: String,
    error: Option<String>,
    hecho: bool,
    valido: bool,
}

// ------------------------------------------------------------------ lista y creación

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
struct FilaCuenta {
    id: i64,
    email: String,
    role: String,
    creada: String,
    disabled_at: Option<String>,
    vaults: i64,
    ultima: Option<String>,
}

async fn cuentas_lista(
    State(estado): State<EstadoServidor>,
    SesionAdmin(sesion): SesionAdmin,
    Query(params): Query<ParamsLista>,
    // `flash`: mensajes post-generación de reset (render directo, sin PRG).
    headers: HeaderMap,
) -> Response {
    pintar_cuentas(
        &estado,
        &sesion,
        params.pagina.unwrap_or(1).max(1),
        params.error,
        params.ok,
        None,
        &headers,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn pintar_cuentas(
    estado: &EstadoServidor,
    sesion: &sesiones::SesionActiva,
    pagina: i64,
    error: Option<String>,
    ok: Option<String>,
    reset_link: Option<String>,
    _headers: &HeaderMap,
) -> Response {
    let offset = (pagina - 1) * POR_PAGINA;

    let filas = match sqlx::query_as::<_, FilaCuenta>(
        "SELECT a.id, a.email, a.role, a.created_at AS creada, a.disabled_at,
                (SELECT COUNT(*) FROM vaults v WHERE v.account_id = a.id) AS vaults,
                (SELECT MAX(s.last_seen) FROM sessions s WHERE s.account_id = a.id) AS ultima
         FROM accounts a
         ORDER BY a.id
         LIMIT ? OFFSET ?",
    )
    .bind(POR_PAGINA + 1)
    .bind(offset)
    .fetch_all(&estado.pool)
    .await
    {
        Ok(filas) => filas,
        Err(error) => return panel::error_interno("cuentas_lista", error),
    };

    let hay_mas = filas.len() as i64 > POR_PAGINA;
    let cuentas = filas
        .into_iter()
        .take(POR_PAGINA as usize)
        .map(|fila| VistaCuenta {
            id: fila.id,
            email: fila.email,
            role: fila.role,
            creada: fila.creada,
            deshabilitada: fila.disabled_at.is_some(),
            es_yo: fila.id == sesion.account_id,
            vaults: fila.vaults,
            ultima: fila.ultima,
        })
        .collect();

    let t = textos::t();
    panel::render(
        || {
            PlantillaCuentas {
                t,
                seccion: "cuentas",
                seccion_titulo: t.nav_cuentas,
                version: VERSION,
                email: sesion.email.clone(),
                rol: sesion.role.clone(),
                csrf: sesion.csrf_token.clone(),
                cuentas,
                pagina,
                hay_mas,
                error,
                ok,
                reset_link,
            }
            .render()
        },
        "cuentas",
    )
}

#[derive(Debug, Deserialize)]
struct FormCrearCuenta {
    #[serde(default)]
    csrf: String,
    email: String,
    password: String,
    rol: String,
}

async fn cuentas_crear(
    State(estado): State<EstadoServidor>,
    headers: HeaderMap,
    SesionAdmin(sesion): SesionAdmin,
    Form(formulario): Form<FormCrearCuenta>,
) -> Response {
    let ip = panel::ip_del_cliente(&headers);
    let t = textos::t();

    let volver = |mensaje: &str, es_error: bool| {
        let clave = if es_error { "error" } else { "ok" };
        Redirect::to(&format!("/admin/cuentas?{clave}={}", flash(mensaje))).into_response()
    };

    if !sesiones::csrf_valido(&sesion, &formulario.csrf) {
        return (StatusCode::FORBIDDEN, "CSRF inválido").into_response();
    }

    let email = formulario.email.trim().to_lowercase();
    let rol = if formulario.rol == "admin" { "admin" } else { "user" };

    if !email.contains('@') || email.len() > 254 {
        return volver(t.msg_email_invalido, true);
    }
    if formulario.password.chars().count() < 8 {
        return volver(t.msg_password_corta, true);
    }

    let creada = match auth::crear_cuenta(&estado.pool, &email, &formulario.password).await {
        Ok(cuenta) => cuenta,
        Err(_) => return volver(t.msg_cuenta_ya_existe, true),
    };

    if rol == "admin" {
        let _ = sqlx::query("UPDATE accounts SET role = 'admin' WHERE id = ?")
            .bind(creada.id)
            .execute(&estado.pool)
            .await;
    }

    sesiones::auditar(
        &estado.pool,
        Some(sesion.account_id),
        "cuenta_creada",
        Some(&format!("#{} {email} ({rol})", creada.id)),
        ip.as_deref(),
    )
    .await;

    volver(t.msg_cuenta_creada, false)
}

// ------------------------------------------------------------------ acciones por fila

#[derive(Debug, Deserialize)]
struct FormAccion {
    #[serde(default)]
    csrf: String,
    id: i64,
    accion: String,
}

async fn cuentas_accion(
    State(estado): State<EstadoServidor>,
    headers: HeaderMap,
    SesionAdmin(sesion): SesionAdmin,
    Form(formulario): Form<FormAccion>,
) -> Response {
    let ip = panel::ip_del_cliente(&headers);
    let t = textos::t();

    let volver = |mensaje: String, es_error: bool| {
        let clave = if es_error { "error" } else { "ok" };
        Redirect::to(&format!("/admin/cuentas?{clave}={}", flash(&mensaje))).into_response()
    };

    if !sesiones::csrf_valido(&sesion, &formulario.csrf) {
        return (StatusCode::FORBIDDEN, "CSRF inválido").into_response();
    }

    let target = match sqlx::query_as::<_, (i64, String, String, Option<String>)>(
        "SELECT id, email, role, disabled_at FROM accounts WHERE id = ?",
    )
    .bind(formulario.id)
    .fetch_optional(&estado.pool)
    .await
    {
        Ok(Some(fila)) => fila,
        Ok(None) => return volver(t.msg_cuenta_no_existe.to_owned(), true),
        Err(error) => return panel::error_interno("cuentas_accion_target", error),
    };

    let admins_activos: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM accounts WHERE role = 'admin' AND disabled_at IS NULL",
    )
    .fetch_one(&estado.pool)
    .await
    .unwrap_or(0);

    let es_ultimo_admin = target.2 == "admin" && target.3.is_none() && admins_activos <= 1;
    let soy_yo = target.0 == sesion.account_id;

    let resultado: std::result::Result<&str, String> = match formulario.accion.as_str() {
        "deshabilitar" => {
            if soy_yo {
                Err(t.msg_no_podes_deshabilitarte.to_owned())
            } else if es_ultimo_admin {
                Err(t.msg_ultimo_admin.to_owned())
            } else {
                sqlx::query("UPDATE accounts SET disabled_at = datetime('now') WHERE id = ?")
                    .bind(target.0)
                    .execute(&estado.pool)
                    .await
                    .map(|_| t.msg_cuenta_deshabilitada)
                    .map_err(|error| error.to_string())
                    .map_err(|error| {
                        let _ = error;
                        t.msg_error_deshabilitar.to_owned()
                    })
            }
        }
        "habilitar" => sqlx::query("UPDATE accounts SET disabled_at = NULL WHERE id = ?")
            .bind(target.0)
            .execute(&estado.pool)
            .await
            .map(|_| t.msg_cuenta_habilitada)
            .map_err(|_| t.msg_error_habilitar.to_owned()),
        "promover" => sqlx::query("UPDATE accounts SET role = 'admin' WHERE id = ?")
            .bind(target.0)
            .execute(&estado.pool)
            .await
            .map(|_| t.msg_rol_admin)
            .map_err(|_| t.msg_error_promover.to_owned()),
        "degradar" => {
            if es_ultimo_admin {
                Err(t.msg_ultimo_admin.to_owned())
            } else {
                sqlx::query("UPDATE accounts SET role = 'user' WHERE id = ?")
                    .bind(target.0)
                    .execute(&estado.pool)
                    .await
                    .map(|_| t.msg_rol_user)
                    .map_err(|_| t.msg_error_degradar.to_owned())
            }
        }
        "reset" => {
            let token = auth::token_string(&auth::token_bytes());
            let insercion = sqlx::query(
                "INSERT INTO password_resets (token_hash, account_id, expires_at)
                 VALUES (?, ?, datetime('now', ?))",
            )
            .bind(auth::hash_token(&token))
            .bind(target.0)
            .bind(format!("+{RESET_VIGENCIA_HORAS} hours"))
            .execute(&estado.pool)
            .await;

            if let Err(error) = insercion {
                return panel::error_interno("cuentas_reset_insert", error);
            }

            sesiones::auditar(
                &estado.pool,
                Some(sesion.account_id),
                "reset_generado",
                Some(&format!("#{} {}", target.0, target.1)),
                ip.as_deref(),
            )
            .await;

            let host = headers
                .get("host")
                .and_then(|valor| valor.to_str().ok())
                .unwrap_or("localhost");
            let esquema = if panel::es_https(&headers) { "https" } else { "http" };
            let link = format!("{esquema}://{host}/reset?token={token}");

            // El link se muestra UNA vez: render directo, sin PRG.
            return pintar_cuentas(
                &estado,
                &sesion,
                1,
                None,
                Some(format!(
                    "Link de reset para {} (copialo y pasáselo: se muestra UNA vez, vale {RESET_VIGENCIA_HORAS} h).",
                    target.1
                )),
                Some(link),
                &headers,
            )
            .await;
        }
        _ => Err(t.msg_accion_desconocida.to_owned()),
    };

    match resultado {
        Ok(mensaje) => {
            if formulario.accion == "deshabilitar" {
                let _ = sesiones::revocar_todas(&estado.pool, target.0).await;
            }
            sesiones::auditar(
                &estado.pool,
                Some(sesion.account_id),
                &format!("cuenta_{}", formulario.accion),
                Some(&format!("#{} {}", target.0, target.1)),
                ip.as_deref(),
            )
            .await;
            volver(mensaje.to_owned(), false)
        }
        Err(mensaje) => volver(mensaje, true),
    }
}

// ------------------------------------------------------------------ borrado de cuenta

#[derive(Debug, Deserialize)]
struct ParamsCuenta {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    error: Option<String>,
}

async fn cuenta_borrar_form(
    State(estado): State<EstadoServidor>,
    SesionAdmin(sesion): SesionAdmin,
    Query(params): Query<ParamsCuenta>,
) -> Response {
    let Some(id) = params.id else {
        return Redirect::to("/admin/cuentas").into_response();
    };

    let cuenta = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT id, email, role FROM accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&estado.pool)
    .await;

    let (id, email_cuenta, rol) = match cuenta {
        Ok(Some(fila)) => fila,
        Ok(None) => return Redirect::to("/admin/cuentas").into_response(),
        Err(error) => return panel::error_interno("cuenta_borrar_form", error),
    };

    let vaults = sqlx::query_as::<_, (i64, String)>(
        "SELECT id, name FROM vaults WHERE account_id = ? ORDER BY id",
    )
    .bind(id)
    .fetch_all(&estado.pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(id, name)| VistaVaultDeCuenta { id, name })
    .collect();

    panel::render(
        || {
            PlantillaCuentaBorrar {
                t: textos::t(),
                seccion: "cuentas",
                seccion_titulo: textos::t().nav_cuentas,
                version: VERSION,
                email: sesion.email.clone(),
                rol: sesion.role.clone(),
                csrf: sesion.csrf_token.clone(),
                cuenta_id: id,
                cuenta_email: email_cuenta,
                cuenta_rol: rol,
                vaults,
                error: params.error,
            }
            .render()
        },
        "cuenta_borrar",
    )
}

#[derive(Debug, Deserialize)]
struct FormBorrarCuenta {
    #[serde(default)]
    csrf: String,
    id: i64,
    confirm: String,
    #[serde(default)]
    borrar_vaults: Option<String>,
}

async fn cuenta_borrar_post(
    State(estado): State<EstadoServidor>,
    headers: HeaderMap,
    SesionAdmin(sesion): SesionAdmin,
    Form(formulario): Form<FormBorrarCuenta>,
) -> Response {
    let ip = panel::ip_del_cliente(&headers);
    let t = textos::t();

    let volver_error = |mensaje: &str| {
        Redirect::to(&format!(
            "/admin/cuentas/borrar?id={}&error={}",
            formulario.id,
            flash(mensaje)
        ))
        .into_response()
    };

    if !sesiones::csrf_valido(&sesion, &formulario.csrf) {
        return (StatusCode::FORBIDDEN, "CSRF inválido").into_response();
    }

    let target = match sqlx::query_as::<_, (i64, String, String, Option<String>)>(
        "SELECT id, email, role, disabled_at FROM accounts WHERE id = ?",
    )
    .bind(formulario.id)
    .fetch_optional(&estado.pool)
    .await
    {
        Ok(Some(fila)) => fila,
        Ok(None) => return Redirect::to("/admin/cuentas").into_response(),
        Err(error) => return panel::error_interno("cuenta_borrar_target", error),
    };

    if target.0 == sesion.account_id {
        return volver_error(t.msg_no_podes_borrarte);
    }
    if formulario.confirm.trim().to_lowercase() != target.1.to_lowercase() {
        return volver_error(t.msg_email_no_coincide);
    }

    let admins_activos: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM accounts WHERE role = 'admin' AND disabled_at IS NULL",
    )
    .fetch_one(&estado.pool)
    .await
    .unwrap_or(0);
    if target.2 == "admin" && target.3.is_none() && admins_activos <= 1 {
        return volver_error(t.msg_ultimo_admin);
    }

    let vaults: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM vaults WHERE account_id = ?")
            .bind(target.0)
            .fetch_all(&estado.pool)
            .await
            .unwrap_or_default();

    if !vaults.is_empty() && formulario.borrar_vaults.as_deref() != Some("si") {
        return volver_error(t.msg_vaults_sin_confirmar);
    }

    // Con la confirmación explícita, se purgan sus vaults (cascada + barrido del CAS).
    for (vault_id, nombre) in &vaults {
        if let Err(error) = purgar_vault(&estado.pool, &estado.cas, *vault_id).await {
            return panel::error_interno("cuenta_borrar_vault", error);
        }
        sesiones::auditar(
            &estado.pool,
            Some(sesion.account_id),
            "vault_delete",
            Some(&format!("#{vault_id} «{nombre}» (borrado con la cuenta #{})", target.0)),
            ip.as_deref(),
        )
        .await;
    }

    if let Err(error) = sqlx::query("DELETE FROM accounts WHERE id = ?")
        .bind(target.0)
        .execute(&estado.pool)
        .await
    {
        return panel::error_interno("cuenta_borrar_delete", error);
    }

    sesiones::auditar(
        &estado.pool,
        Some(sesion.account_id),
        "cuenta_borrada",
        Some(&format!("#{} {} ({} vaults)", target.0, target.1, vaults.len())),
        ip.as_deref(),
    )
    .await;

    Redirect::to(&format!("/admin/cuentas?ok={}", flash(t.msg_cuenta_borrada))).into_response()
}

// ------------------------------------------------------------------ ajustes

async fn registro_abierto(pool: &sqlx::SqlitePool) -> bool {
    sqlx::query_scalar::<_, String>("SELECT value FROM server_settings WHERE key = 'registro_abierto'")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .map(|valor| valor == "on")
        .unwrap_or(false)
}

#[derive(Debug, Deserialize)]
struct ParamsAjustes {
    #[serde(default)]
    ok: Option<String>,
}

async fn ajustes_form(
    State(estado): State<EstadoServidor>,
    SesionAdmin(sesion): SesionAdmin,
    Query(params): Query<ParamsAjustes>,
) -> Response {
    pintar_ajustes(&estado, &sesion, params.ok).await
}

async fn pintar_ajustes(
    estado: &EstadoServidor,
    sesion: &sesiones::SesionActiva,
    ok: Option<String>,
) -> Response {
    let abierto = registro_abierto(&estado.pool).await;

    panel::render(
        || {
            PlantillaAjustes {
                t: textos::t(),
                seccion: "ajustes",
                seccion_titulo: textos::t().nav_ajustes,
                version: VERSION,
                email: sesion.email.clone(),
                rol: sesion.role.clone(),
                csrf: sesion.csrf_token.clone(),
                registro_abierto: abierto,
                ok,
            }
            .render()
        },
        "ajustes",
    )
}

#[derive(Debug, Deserialize)]
struct FormAjustes {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    registro: Option<String>,
}

async fn ajustes_post(
    State(estado): State<EstadoServidor>,
    headers: HeaderMap,
    SesionAdmin(sesion): SesionAdmin,
    Form(formulario): Form<FormAjustes>,
) -> Response {
    if !sesiones::csrf_valido(&sesion, &formulario.csrf) {
        return (StatusCode::FORBIDDEN, "CSRF inválido").into_response();
    }

    let valor = if formulario.registro.is_some() { "on" } else { "off" };
    if let Err(error) = sqlx::query("UPDATE server_settings SET value = ? WHERE key = 'registro_abierto'")
        .bind(valor)
        .execute(&estado.pool)
        .await
    {
        return panel::error_interno("ajustes_update", error);
    }

    sesiones::auditar(
        &estado.pool,
        Some(sesion.account_id),
        "ajuste_cambiado",
        Some(&format!("registro_abierto={valor}")),
        panel::ip_del_cliente(&headers).as_deref(),
    )
    .await;

    Redirect::to(&format!(
        "/admin/ajustes?ok={}",
        flash(textos::t().msg_ajustes_guardados)
    ))
    .into_response()
}

// ------------------------------------------------------------------ registro público

async fn register_form(State(estado): State<EstadoServidor>) -> Response {
    let abierto = registro_abierto(&estado.pool).await;
    panel::render(
        || {
            PlantillaRegister {
                t: textos::t(),
                abierto,
                error: None,
                hecho: false,
            }
            .render()
        },
        "register",
    )
}

#[derive(Debug, Deserialize)]
struct FormRegister {
    email: String,
    password: String,
    repetir: String,
}

async fn register_post(
    State(estado): State<EstadoServidor>,
    headers: HeaderMap,
    Form(formulario): Form<FormRegister>,
) -> Response {
    let abierto = registro_abierto(&estado.pool).await;
    let t = textos::t();

    let pintar = |error: Option<String>, hecho: bool| {
        panel::render(
            || {
                PlantillaRegister {
                    t: textos::t(),
                    abierto,
                    error,
                    hecho,
                }
                .render()
            },
            "register",
        )
    };

    if !abierto {
        return pintar(Some(t.msg_registro_cerrado.to_owned()), false);
    }

    // Freno simple por IP (mismo limitador que el login): 5 altas por minuto.
    let clave = format!("registro:{}", panel::ip_del_cliente(&headers).unwrap_or_default());
    if !estado.limitador_login.permitir(&clave) {
        return pintar(Some(t.msg_demasiados_intentos.to_owned()), false);
    }
    estado.limitador_login.registrar(&clave);

    let email = formulario.email.trim().to_lowercase();
    if !email.contains('@') || email.len() > 254 {
        return pintar(Some(t.msg_email_invalido.to_owned()), false);
    }
    if formulario.password != formulario.repetir {
        return pintar(Some(t.msg_passwords_no_coinciden.to_owned()), false);
    }
    if formulario.password.chars().count() < 8 {
        return pintar(Some(t.msg_password_corta.to_owned()), false);
    }

    let creada = match auth::crear_cuenta(&estado.pool, &email, &formulario.password).await {
        Ok(cuenta) => cuenta,
        Err(_) => return pintar(Some(t.msg_cuenta_ya_existe.to_owned()), false),
    };

    sesiones::auditar(
        &estado.pool,
        Some(creada.id),
        "cuenta_registrada",
        Some(&email),
        panel::ip_del_cliente(&headers).as_deref(),
    )
    .await;

    pintar(None, true)
}

// ------------------------------------------------------------------ reset público

#[derive(Debug, Deserialize)]
struct ParamsReset {
    #[serde(default)]
    token: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct FilaReset {
    account_id: i64,
    email: String,
}

/// Busca un reset válido (sin usar y sin vencer) por el token crudo.
async fn reset_valido(pool: &sqlx::SqlitePool, token: &str) -> Option<FilaReset> {
    sqlx::query_as::<_, FilaReset>(
        "SELECT r.account_id, a.email
         FROM password_resets r
         JOIN accounts a ON a.id = r.account_id
         WHERE r.token_hash = ? AND r.used_at IS NULL AND datetime('now') < r.expires_at
         ORDER BY r.id DESC
         LIMIT 1",
    )
    .bind(auth::hash_token(token))
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

async fn reset_form(State(estado): State<EstadoServidor>, Query(params): Query<ParamsReset>) -> Response {
    let token = params.token.unwrap_or_default();
    let fila = reset_valido(&estado.pool, &token).await;

    match fila {
        Some(fila) => panel::render(
            || {
                PlantillaReset {
                    t: textos::t(),
                    token: token.clone(),
                    email: fila.email.clone(),
                    error: None,
                    hecho: false,
                    valido: true,
                }
                .render()
            },
            "reset",
        ),
        None => panel::render(
            || {
                PlantillaReset {
                    t: textos::t(),
                    token: String::new(),
                    email: String::new(),
                    error: None,
                    hecho: false,
                    valido: false,
                }
                .render()
            },
            "reset",
        ),
    }
}

#[derive(Debug, Deserialize)]
struct FormReset {
    token: String,
    nueva: String,
    repetir: String,
}

async fn reset_post(
    State(estado): State<EstadoServidor>,
    headers: HeaderMap,
    Form(formulario): Form<FormReset>,
) -> Response {
    let t = textos::t();

    let pintar = |error: Option<String>, hecho: bool, valido: bool, email: String| {
        panel::render(
            || {
                PlantillaReset {
                    t: textos::t(),
                    token: formulario.token.clone(),
                    email,
                    error,
                    hecho,
                    valido,
                }
                .render()
            },
            "reset",
        )
    };

    let Some(fila) = reset_valido(&estado.pool, &formulario.token).await else {
        return pintar(None, false, false, String::new());
    };

    if formulario.nueva != formulario.repetir {
        return pintar(
            Some(t.msg_passwords_no_coinciden.to_owned()),
            false,
            true,
            fila.email,
        );
    }
    if formulario.nueva.chars().count() < 8 {
        return pintar(Some(t.msg_password_corta.to_owned()), false, true, fila.email);
    }

    let hash = match auth::hash_password(&formulario.nueva) {
        Ok(hash) => hash,
        Err(error) => return panel::error_interno("reset_hash", error),
    };

    let resultado = sqlx::query("UPDATE accounts SET password_hash = ? WHERE id = ?")
        .bind(hash)
        .bind(fila.account_id)
        .execute(&estado.pool)
        .await;
    if let Err(error) = resultado {
        return panel::error_interno("reset_update", error);
    }

    let _ = sqlx::query("UPDATE password_resets SET used_at = datetime('now') WHERE token_hash = ?")
        .bind(auth::hash_token(&formulario.token))
        .execute(&estado.pool)
        .await;

    // Emergencia (password comprometida): mueren TODAS las sesiones web y los tokens de los
    // devices de la cuenta. Los clientes vuelven a loguear con la password nueva.
    let _ = sesiones::revocar_todas(&estado.pool, fila.account_id).await;
    let _ = sqlx::query("DELETE FROM device_tokens WHERE account_id = ?")
        .bind(fila.account_id)
        .execute(&estado.pool)
        .await;

    sesiones::auditar(
        &estado.pool,
        Some(fila.account_id),
        "password_reseteada",
        None,
        panel::ip_del_cliente(&headers).as_deref(),
    )
    .await;

    pintar(None, true, true, fila.email)
}

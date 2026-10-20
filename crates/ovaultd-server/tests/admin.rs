//! Tests de integración del panel de administración (ADR-0017, corte W0).
//!
//! Cubren lo que W0 promete: login con cookie (flags correctos), autorización de `/admin`,
//! CSRF en todos los POST con sesión, freno de intentos, expiración, logout y el cambio de
//! password de la propia cuenta (con revocación de las demás sesiones). El flujo del
//! bootstrap (`/setup`) tiene sus propios tests en `setup.rs`.

mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use common::{response_bytes, EMAIL, PASSWORD};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// ------------------------------------------------------------------ helpers

fn post_form(uri: &str, cuerpo: &str) -> Result<Request<Body>> {
    Ok(Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(cuerpo.to_owned()))?)
}

fn post_form_con_cookie(uri: &str, cuerpo: &str, cookie: &str) -> Result<Request<Body>> {
    Ok(Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, cookie)
        .body(Body::from(cuerpo.to_owned()))?)
}

fn get_con_cookie(uri: &str, cookie: &str) -> Result<Request<Body>> {
    Ok(Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())?)
}

/// Extrae `ovaultd_session=…` de un Set-Cookie.
fn cookie_de(response: &axum::response::Response) -> Result<String> {
    let valor = response
        .headers()
        .get(header::SET_COOKIE)
        .ok_or("sin set-cookie")?
        .to_str()?;

    Ok(valor
        .split(';')
        .next()
        .ok_or("set-cookie vacío")?
        .to_owned())
}

async fn cuerpo(response: axum::response::Response) -> Result<String> {
    Ok(String::from_utf8(response_bytes(response).await?)?)
}

/// Login completo devolviendo la cookie de sesión.
async fn login(contexto: &common::TestContext) -> Result<String> {
    let response = contexto
        .request(post_form(
            "/admin/login",
            &format!("email={EMAIL}&password={PASSWORD}"),
        )?)
        .await?;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    cookie_de(&response)
}

/// El CSRF de la última sesión creada (vive crudo en la tabla: tiene que poder reimprimirse).
async fn csrf_de_la_ultima_sesion(contexto: &common::TestContext) -> Result<String> {
    Ok(
        sqlx::query_scalar("SELECT csrf_token FROM sessions ORDER BY id DESC LIMIT 1")
            .fetch_one(&contexto.pool)
            .await?,
    )
}

// ------------------------------------------------------------------ acceso

#[tokio::test]
async fn sin_sesion_el_panel_redirige_al_login() -> Result<()> {
    let contexto = common::TestContext::new().await?;

    let response = contexto
        .request(Request::get("/admin").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|valor| valor.to_str().ok()),
        Some("/admin/login")
    );

    // El formulario de login sí es público.
    let response = contexto
        .request(Request::get("/admin/login").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let html = cuerpo(response).await?;
    assert!(html.contains("Entrar al panel"));
    assert!(html.contains("action=\"/admin/login\""));

    // La raíz con cuentas lleva al panel.
    let response = contexto
        .request(Request::get("/").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|valor| valor.to_str().ok()),
        Some("/admin")
    );

    Ok(())
}

// ------------------------------------------------------------------ login

#[tokio::test]
async fn login_end_to_end_con_cookie_httponly() -> Result<()> {
    let contexto = common::TestContext::new().await?;

    let response = contexto
        .request(post_form(
            "/admin/login",
            &format!("email={EMAIL}&password={PASSWORD}"),
        )?)
        .await?;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|valor| valor.to_str().ok()),
        Some("/admin")
    );

    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .ok_or("sin set-cookie")?
        .to_str()?;
    assert!(set_cookie.contains("ovaultd_session="));
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Lax"));
    assert!(!set_cookie.contains("Secure")); // sin X-Forwarded-Proto: auto = sin Secure

    let cookie = cookie_de(&response)?;

    // Con la cookie, el dashboard responde.
    let response = contexto.request(get_con_cookie("/admin", &cookie)?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let html = cuerpo(response).await?;
    assert!(html.contains("Panel"));
    assert!(html.contains(EMAIL));
    assert!(html.contains("name=\"csrf\""));

    // La auditoría tiene el login ok.
    let logins: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'login_ok'")
            .fetch_one(&contexto.pool)
            .await?;
    assert_eq!(logins, 1);

    Ok(())
}

#[tokio::test]
async fn con_https_la_cookie_lleva_secure() -> Result<()> {
    let contexto = common::TestContext::new().await?;

    let request = Request::builder()
        .method(Method::POST)
        .uri("/admin/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("x-forwarded-proto", "https")
        .body(Body::from(format!(
            "email={EMAIL}&password={PASSWORD}"
        )))?;

    let response = contexto.request(request).await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .ok_or("sin set-cookie")?
        .to_str()?;
    assert!(set_cookie.contains("Secure"));

    Ok(())
}

#[tokio::test]
async fn login_fallido_no_da_cookie_y_queda_auditado() -> Result<()> {
    let contexto = common::TestContext::new().await?;

    let response = contexto
        .request(post_form(
            "/admin/login",
            &format!("email={EMAIL}&password=password-incorrecta"),
        )?)
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().get(header::SET_COOKIE).is_none());

    let fallidos: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'login_fallido'")
            .fetch_one(&contexto.pool)
            .await?;
    assert_eq!(fallidos, 1);

    // Un email inexistente tampoco entra (y no distingue: mismo 401).
    let response = contexto
        .request(post_form(
            "/admin/login",
            &format!("email=nadie@example.com&password={PASSWORD}"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn el_freno_corta_los_intentos() -> Result<()> {
    let contexto = common::TestContext::new().await?;

    for _ in 0..5 {
        let response = contexto
            .request(post_form(
                "/admin/login",
                &format!("email={EMAIL}&password=password-mala"),
            )?)
            .await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // El sexto ya no toca Argon2: 429 con Retry-After.
    let response = contexto
        .request(post_form(
            "/admin/login",
            &format!("email={EMAIL}&password=password-mala"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(response.headers().get(header::RETRY_AFTER).is_some());

    // Y el login bueno también queda frenado mientras dure la ventana.
    let response = contexto
        .request(post_form(
            "/admin/login",
            &format!("email={EMAIL}&password={PASSWORD}"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    Ok(())
}

// ------------------------------------------------------------------ sesión

#[tokio::test]
async fn la_sesion_vencida_no_vale() -> Result<()> {
    let contexto = common::TestContext::new().await?;
    let cookie = login(&contexto).await?;

    sqlx::query("UPDATE sessions SET expires_at = datetime('now', '-1 hour')")
        .execute(&contexto.pool)
        .await?;

    let response = contexto.request(get_con_cookie("/admin", &cookie)?).await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|valor| valor.to_str().ok()),
        Some("/admin/login")
    );

    Ok(())
}

#[tokio::test]
async fn logout_revoca_la_sesion_y_expira_la_cookie() -> Result<()> {
    let contexto = common::TestContext::new().await?;
    let cookie = login(&contexto).await?;
    let csrf = csrf_de_la_ultima_sesion(&contexto).await?;

    let response = contexto
        .request(post_form_con_cookie(
            "/admin/logout",
            &format!("csrf={csrf}"),
            &cookie,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .ok_or("sin set-cookie")?
        .to_str()?;
    assert!(set_cookie.contains("Max-Age=0"));

    // La cookie vieja ya no vale.
    let response = contexto.request(get_con_cookie("/admin", &cookie)?).await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    // Y la sesión se fue de la base.
    let sesiones: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&contexto.pool)
        .await?;
    assert_eq!(sesiones, 0);

    Ok(())
}

// ------------------------------------------------------------------ csrf

#[tokio::test]
async fn los_post_con_sesion_exigen_csrf() -> Result<()> {
    let contexto = common::TestContext::new().await?;
    let cookie = login(&contexto).await?;

    let response = contexto
        .request(post_form_con_cookie("/admin/logout", "csrf=inventado", &cookie)?)
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = contexto
        .request(post_form_con_cookie("/admin/logout", "", &cookie)?)
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = contexto
        .request(post_form_con_cookie(
            "/admin/mi-cuenta",
            "csrf=inventado&actual=a&nueva=bbbbbbbb&repetir=bbbbbbbb",
            &cookie,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // La sesión sigue viva (nada se revocó).
    let sesiones: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&contexto.pool)
        .await?;
    assert_eq!(sesiones, 1);

    Ok(())
}

// ------------------------------------------------------------------ stats

#[tokio::test]
async fn las_estadisticas_muestran_las_notas_reales() -> Result<()> {
    let contexto = common::TestContext::new().await?;

    // Una nota REAL como la sube un cliente: blob registrado + batch firmado.
    contexto.registrar_blob(7, 91).await?;
    let commit = contexto.commit(vec![common::add_op(1, 7, 91)], 0).await;
    assert!(commit.is_ok());

    let cookie = login(&contexto).await?;

    // Dashboard global: aparecen los contadores nuevos.
    let html = cuerpo(contexto.request(get_con_cookie("/admin", &cookie)?).await?).await?;
    assert!(html.contains(">Archivos<"));
    assert!(html.contains(">Almacenado<"));

    // /admin/vaults: el vault real con sus notas y el espacio (91 B del blob).
    let response = contexto
        .request(get_con_cookie("/admin/vaults", &cookie)?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let html = cuerpo(response).await?;
    assert!(html.contains("vault-de-integracion"));
    assert!(html.contains(">91 B</td>"));

    Ok(())
}

#[tokio::test]
async fn vaults_sin_sesion_redirige_al_login() -> Result<()> {
    let contexto = common::TestContext::new().await?;

    let response = contexto
        .request(Request::get("/admin/vaults").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|valor| valor.to_str().ok()),
        Some("/admin/login")
    );

    Ok(())
}

#[tokio::test]
async fn cambiar_password_valida_y_revoca_las_demas_sesiones() -> Result<()> {
    let contexto = common::TestContext::new().await?;
    let cookie_1 = login(&contexto).await?;
    let csrf_1 = csrf_de_la_ultima_sesion(&contexto).await?;
    let cookie_2 = login(&contexto).await?;

    // Sin la actual correcta, no hay cambio.
    let response = contexto
        .request(post_form_con_cookie(
            "/admin/mi-cuenta",
            &format!("csrf={csrf_1}&actual=mala&nueva=password-nueva-ok&repetir=password-nueva-ok"),
            &cookie_1,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(cuerpo(response).await?.contains("no es correcta"));

    // Repetición que no coincide: tampoco.
    let response = contexto
        .request(post_form_con_cookie(
            "/admin/mi-cuenta",
            &format!(
                "csrf={csrf_1}&actual={PASSWORD}&nueva=password-nueva-ok&repetir=otra-distinta"
            ),
            &cookie_1,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(cuerpo(response).await?.contains("no coinciden"));

    // El cambio de verdad.
    let response = contexto
        .request(post_form_con_cookie(
            "/admin/mi-cuenta",
            &format!(
                "csrf={csrf_1}&actual={PASSWORD}&nueva=password-nueva-ok&repetir=password-nueva-ok"
            ),
            &cookie_1,
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(cuerpo(response).await?.contains("Password cambiada"));

    // La sesión 2 murió; la 1 (la que cambió) sigue viva.
    let response = contexto
        .request(get_con_cookie("/admin", &cookie_2)?)
        .await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let response = contexto
        .request(get_con_cookie("/admin", &cookie_1)?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);

    // Login: la vieja no entra, la nueva sí.
    let response = contexto
        .request(post_form(
            "/admin/login",
            &format!("email={EMAIL}&password={PASSWORD}"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = contexto
        .request(post_form(
            "/admin/login",
            &format!("email={EMAIL}&password=password-nueva-ok"),
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    // Auditoría del cambio.
    let cambios: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'password_cambiada'")
            .fetch_one(&contexto.pool)
            .await?;
    assert_eq!(cambios, 1);

    Ok(())
}

// ------------------------------------------------------------------ W1: cuentas

use common::TestContext;
use ovaultd_server::auth;

/// Promueve la cuenta del contexto a admin (los tests de W1 lo necesitan).
async fn hacer_admin(contexto: &common::TestContext) -> Result<()> {
    sqlx::query("UPDATE accounts SET role = 'admin' WHERE email = ?")
        .bind(EMAIL)
        .execute(&contexto.pool)
        .await?;
    Ok(())
}

fn post_json(uri: &str, cuerpo: &str) -> Result<Request<Body>> {
    Ok(Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(cuerpo.to_owned()))?)
}

/// Extrae el token del link de reset que aparece en el HTML de /admin/cuentas.
fn token_del_link(html: &str) -> Option<String> {
    let resto = html.split("/reset?token=").nth(1)?;
    let token: String = resto
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

#[tokio::test]
async fn cuentas_es_solo_para_admins() -> Result<()> {
    let contexto = TestContext::new().await?;
    let cookie = login(&contexto).await?;

    // Cuenta 'user' (el default): la página es 403 (hay sesión, falta el rol).
    let resp = contexto
        .request(get_con_cookie("/admin/cuentas", &cookie)?)
        .await?;
    let status = resp.status();
    let html = cuerpo(resp).await?;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "página devuelta: {}",
        &html[..400.min(html.len())]
    );

    // Promovida a admin: 200, y la tabla la muestra.
    hacer_admin(&contexto).await?;
    let resp = contexto
        .request(get_con_cookie("/admin/cuentas", &cookie)?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = cuerpo(resp).await?;
    assert!(html.contains(EMAIL), "la tabla lista la cuenta propia");

    Ok(())
}

#[tokio::test]
async fn crear_cuenta_desde_el_panel_y_login_de_la_nueva() -> Result<()> {
    let contexto = TestContext::new().await?;
    hacer_admin(&contexto).await?;
    let cookie = login(&contexto).await?;
    let csrf = csrf_de_la_ultima_sesion(&contexto).await?;

    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/cuentas/crear",
            &format!(
                "csrf={csrf}&email=nueva%40ejemplo.com&password=password-de-prueba&rol=user"
            ),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    // La nueva cuenta existe y puede loguear en el panel.
    let resp = contexto
        .request(post_form(
            "/admin/login",
            "email=nueva%40ejemplo.com&password=password-de-prueba",
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "login de la cuenta nueva");

    // Y queda auditada.
    let eventos: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action = 'cuenta_creada'",
    )
    .fetch_one(&contexto.pool)
    .await?;
    assert_eq!(eventos, 1);

    Ok(())
}

#[tokio::test]
async fn deshabilitar_corta_el_login_web_y_el_api() -> Result<()> {
    let contexto = TestContext::new().await?;
    let otra = auth::crear_cuenta(&contexto.pool, "otra@ejemplo.com", PASSWORD).await?;

    // Antes: la API la deja entrar.
    let resp = contexto
        .request(post_json(
            "/api/v1/auth/signin",
            &format!(
                r#"{{"email":"otra@ejemplo.com","password":"{PASSWORD}","name":"dev"}}"#
            ),
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK, "signin antes de deshabilitar");

    hacer_admin(&contexto).await?;
    let cookie = login(&contexto).await?;
    let csrf = csrf_de_la_ultima_sesion(&contexto).await?;

    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/cuentas/accion",
            &format!("csrf={csrf}&id={}&accion=deshabilitar", otra.id),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    // Después: la API la rebota…
    let resp = contexto
        .request(post_json(
            "/api/v1/auth/signin",
            &format!(
                r#"{{"email":"otra@ejemplo.com","password":"{PASSWORD}","name":"dev"}}"#
            ),
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "signin deshabilitada");

    // …y el panel no le da cookie tampoco.
    let resp = contexto
        .request(post_form(
            "/admin/login",
            "email=otra%40ejemplo.com&password=password-de-prueba",
        )?)
        .await?;
    assert!(
        resp.headers().get(header::SET_COOKIE).is_none(),
        "una deshabilitada no recibe cookie de sesión"
    );

    // Habilitar la devuelve al ruedo.
    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/cuentas/accion",
            &format!("csrf={csrf}&id={}&accion=habilitar", otra.id),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let resp = contexto
        .request(post_json(
            "/api/v1/auth/signin",
            &format!(
                r#"{{"email":"otra@ejemplo.com","password":"{PASSWORD}","name":"dev"}}"#
            ),
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK, "signin tras habilitar");

    Ok(())
}

#[tokio::test]
async fn el_ultimo_admin_no_se_degrada_ni_deshabilita() -> Result<()> {
    let contexto = TestContext::new().await?;
    hacer_admin(&contexto).await?;
    let cookie = login(&contexto).await?;
    let csrf = csrf_de_la_ultima_sesion(&contexto).await?;

    let propio_id: i64 = sqlx::query_scalar("SELECT id FROM accounts WHERE email = ?")
        .bind(EMAIL)
        .fetch_one(&contexto.pool)
        .await?;

    // Degradarse siendo el único admin: rebotado, el rol no cambia.
    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/cuentas/accion",
            &format!("csrf={csrf}&id={propio_id}&accion=degradar"),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let destino = resp.headers().get(header::LOCATION).unwrap().to_str()?;
    assert!(destino.contains("error"), "debe volver con error: {destino}");
    let rol: String = sqlx::query_scalar("SELECT role FROM accounts WHERE id = ?")
        .bind(propio_id)
        .fetch_one(&contexto.pool)
        .await?;
    assert_eq!(rol, "admin");

    // Deshabilitarse a sí mismo: rebotado, la cuenta queda activa.
    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/cuentas/accion",
            &format!("csrf={csrf}&id={propio_id}&accion=deshabilitar"),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let deshabilitada: Option<String> =
        sqlx::query_scalar("SELECT disabled_at FROM accounts WHERE id = ?")
            .bind(propio_id)
            .fetch_one(&contexto.pool)
            .await?;
    assert!(deshabilitada.is_none());

    Ok(())
}

#[tokio::test]
async fn borrar_cuenta_exige_confirmacion_y_purga_vaults() -> Result<()> {
    let contexto = TestContext::new().await?;
    let (vault_id, _token) = contexto.agregar_vault_ajeno("con-vaults@ejemplo.com").await?;

    hacer_admin(&contexto).await?;
    let cookie = login(&contexto).await?;
    let csrf = csrf_de_la_ultima_sesion(&contexto).await?;

    let cuenta_id: i64 = sqlx::query_scalar("SELECT id FROM accounts WHERE email = ?")
        .bind("con-vaults@ejemplo.com")
        .fetch_one(&contexto.pool)
        .await?;

    // Sin el email exacto: rebotado.
    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/cuentas/borrar",
            &format!("csrf={csrf}&id={cuenta_id}&confirm=nada"),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let destino = resp.headers().get(header::LOCATION).unwrap().to_str()?;
    assert!(destino.contains("cuentas/borrar"), "vuelve al form: {destino}");

    // Con el email pero sin marcar los vaults: rebotado (todavía tiene datos).
    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/cuentas/borrar",
            &format!("csrf={csrf}&id={cuenta_id}&confirm=con-vaults%40ejemplo.com"),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let destino = resp.headers().get(header::LOCATION).unwrap().to_str()?;
    assert!(destino.contains("vaults"), "exige la confirmación de vaults: {destino}");

    // Completo: cuenta + vault + blobs.
    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/cuentas/borrar",
            &format!(
                "csrf={csrf}&id={cuenta_id}&confirm=con-vaults%40ejemplo.com&borrar_vaults=si"
            ),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let destino = resp.headers().get(header::LOCATION).unwrap().to_str()?;
    assert!(destino.starts_with("/admin/cuentas?ok="), "éxito: {destino}");

    let cuentas: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE id = ?")
        .bind(cuenta_id)
        .fetch_one(&contexto.pool)
        .await?;
    let vaults: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vaults WHERE id = ?")
        .bind(vault_id)
        .fetch_one(&contexto.pool)
        .await?;
    assert_eq!(cuentas, 0);
    assert_eq!(vaults, 0, "el vault se purgó con la cuenta");

    Ok(())
}

#[tokio::test]
async fn reset_password_cambia_y_viejo_login_muere() -> Result<()> {
    let contexto = TestContext::new().await?;
    auth::crear_cuenta(&contexto.pool, "reset@ejemplo.com", PASSWORD).await?;

    hacer_admin(&contexto).await?;
    let cookie = login(&contexto).await?;
    let csrf = csrf_de_la_ultima_sesion(&contexto).await?;

    let cuenta_id: i64 = sqlx::query_scalar("SELECT id FROM accounts WHERE email = ?")
        .bind("reset@ejemplo.com")
        .fetch_one(&contexto.pool)
        .await?;

    // El admin genera el link: se renderiza UNA vez con el token adentro.
    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/cuentas/accion",
            &format!("csrf={csrf}&id={cuenta_id}&accion=reset"),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = cuerpo(resp).await?;
    let token = token_del_link(&html).ok_or("el HTML trae el link de reset")?;

    // El form de reset acepta el token…
    let resp = contexto
        .request(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/reset?token={token}"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = cuerpo(resp).await?;
    assert!(html.contains("reset@ejemplo.com"), "el form dice de quién es");

    // …y el POST deja la password nueva + quema el token.
    let resp = contexto
        .request(post_form(
            "/reset",
            &format!("token={token}&nueva=password-nueva-99&repetir=password-nueva-99"),
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);

    // La nueva entra; la vieja murió.
    let resp = contexto
        .request(post_form(
            "/admin/login",
            "email=reset%40ejemplo.com&password=password-nueva-99",
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "login con la nueva");
    let resp = contexto
        .request(post_form(
            "/admin/login",
            &format!("email=reset%40ejemplo.com&password={PASSWORD}"),
        )?)
        .await?;
    assert!(
        resp.headers().get(header::SET_COOKIE).is_none(),
        "la password vieja ya no entra"
    );

    // Reusar el token: link muerto.
    let resp = contexto
        .request(post_form(
            "/reset",
            &format!("token={token}&nueva=otra-password-77&repetir=otra-password-77"),
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = cuerpo(resp).await?;
    assert!(html.contains("no es válido"), "un token usado no vuelve a valer");

    Ok(())
}

#[tokio::test]
async fn register_respeta_el_toggle_del_admin() -> Result<()> {
    let contexto = TestContext::new().await?;

    // Cerrado (default): el POST no crea nada, la página lo dice.
    let resp = contexto
        .request(post_form(
            "/register",
            "email=colado%40ejemplo.com&password=password-de-prueba&repetir=password-de-prueba",
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = cuerpo(resp).await?;
    assert!(html.contains("cerrado"), "el server avisa que está cerrado");
    let cuentas: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE email = ?")
        .bind("colado@ejemplo.com")
        .fetch_one(&contexto.pool)
        .await?;
    assert_eq!(cuentas, 0, "cerrado = no entra nadie");

    // El admin lo abre desde Ajustes.
    hacer_admin(&contexto).await?;
    let cookie = login(&contexto).await?;
    let csrf = csrf_de_la_ultima_sesion(&contexto).await?;
    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/ajustes",
            &format!("csrf={csrf}&registro=on"),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    // Abierto: el alta funciona y la cuenta nueva entra al panel.
    let resp = contexto
        .request(post_form(
            "/register",
            "email=colado%40ejemplo.com&password=password-de-prueba&repetir=password-de-prueba",
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = cuerpo(resp).await?;
    assert!(html.contains("Cuenta creada"), "alta confirmada");
    let resp = contexto
        .request(post_form(
            "/admin/login",
            "email=colado%40ejemplo.com&password=password-de-prueba",
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER, "la cuenta registrada loguea");

    // Y queda auditado el alta pública.
    let eventos: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action = 'cuenta_registrada'",
    )
    .fetch_one(&contexto.pool)
    .await?;
    assert_eq!(eventos, 1);

    Ok(())
}

// ------------------------------------------------------------------ W2: vaults, devices y auditoría

#[tokio::test]
async fn vault_detalle_muestra_los_numeros_de_la_db() -> Result<()> {
    let contexto = TestContext::new().await?;
    hacer_admin(&contexto).await?;
    let cookie = login(&contexto).await?;

    let resp = contexto
        .request(get_con_cookie(
            &format!("/admin/vaults/detalle?id={}", contexto.vault_id),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = cuerpo(resp).await?;
    assert!(html.contains("vault-de-integracion"), "nombre del vault");
    assert!(html.contains("Archivos vivos"), "las stats del vault están");
    // El link de borrado vive en el detalle.
    assert!(html.contains(&format!("/admin/vaults/borrar?id={}", contexto.vault_id)));

    Ok(())
}

#[tokio::test]
async fn vault_borrar_exige_el_nombre_y_purga() -> Result<()> {
    let contexto = TestContext::new().await?;
    hacer_admin(&contexto).await?;
    let cookie = login(&contexto).await?;
    let csrf = csrf_de_la_ultima_sesion(&contexto).await?;

    // El form existe y muestra el nombre a escribir.
    let resp = contexto
        .request(get_con_cookie(
            &format!("/admin/vaults/borrar?id={}", contexto.vault_id),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);

    // Nombre equivocado: rebotado, el vault sigue.
    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/vaults/borrar",
            &format!("csrf={csrf}&id={}&confirm=nada", contexto.vault_id),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let destino = resp.headers().get(header::LOCATION).unwrap().to_str()?;
    assert!(destino.contains("error"), "rebote con error: {destino}");

    // Nombre exacto: purgado de verdad.
    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/vaults/borrar",
            &format!(
                "csrf={csrf}&id={}&confirm=vault-de-integracion",
                contexto.vault_id
            ),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let destino = resp.headers().get(header::LOCATION).unwrap().to_str()?;
    assert!(destino.starts_with("/admin/vaults?ok="), "éxito: {destino}");

    let vaults: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vaults WHERE id = ?")
        .bind(contexto.vault_id)
        .fetch_one(&contexto.pool)
        .await?;
    assert_eq!(vaults, 0);
    let auditados: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'vault_delete'")
            .fetch_one(&contexto.pool)
            .await?;
    assert_eq!(auditados, 1);

    Ok(())
}

#[tokio::test]
async fn devices_revocar_mata_el_token_y_queda_auditado() -> Result<()> {
    let contexto = TestContext::new().await?;
    hacer_admin(&contexto).await?;
    let cookie = login(&contexto).await?;
    let csrf = csrf_de_la_ultima_sesion(&contexto).await?;

    // Con el token del contexto, la API contesta.
    let resp = contexto
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/vaults")
                .header(header::AUTHORIZATION, format!("Bearer {}", contexto.token))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::OK, "el token vivo entra");

    // La lista de devices lo muestra activo.
    let resp = contexto
        .request(get_con_cookie("/admin/devices", &cookie)?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = cuerpo(resp).await?;
    assert!(html.contains("test-device"));

    // Revocar desde el panel.
    let resp = contexto
        .request(post_form_con_cookie(
            "/admin/devices/revocar",
            &format!("csrf={csrf}&id={}", contexto.device_id),
            &cookie,
        )?)
        .await?;
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    // El MISMO token ya no entra…
    let resp = contexto
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/api/v1/vaults")
                .header(header::AUTHORIZATION, format!("Bearer {}", contexto.token))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "token revocado");

    // …y queda auditado + revocado en la DB.
    let revocado: Option<String> = sqlx::query_scalar("SELECT revoked_at FROM devices WHERE id = ?")
        .bind(contexto.device_id)
        .fetch_one(&contexto.pool)
        .await?;
    assert!(revocado.is_some());
    let auditados: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'device_revocado'")
            .fetch_one(&contexto.pool)
            .await?;
    assert_eq!(auditados, 1);

    Ok(())
}

#[tokio::test]
async fn auditoria_lista_y_filtra() -> Result<()> {
    let contexto = TestContext::new().await?;
    hacer_admin(&contexto).await?;
    let cookie = login(&contexto).await?;

    // La bitácora muestra el login recién hecho.
    let resp = contexto
        .request(get_con_cookie("/admin/auditoria", &cookie)?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = cuerpo(resp).await?;
    assert!(html.contains("login_ok"), "el login queda auditado");

    // Filtro por subcadena.
    let resp = contexto
        .request(get_con_cookie("/admin/auditoria?q=login_ok", &cookie)?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = cuerpo(resp).await?;
    assert!(html.contains("login_ok"));

    // Filtro que no matchea: cero eventos.
    let resp = contexto
        .request(get_con_cookie("/admin/auditoria?q=xyz_inexistente", &cookie)?)
        .await?;
    let html = cuerpo(resp).await?;
    assert!(html.contains("0 evento"), "sin coincidencias: {html:.120}");

    Ok(())
}

#[tokio::test]
async fn las_paginas_w2_son_solo_para_admins() -> Result<()> {
    let contexto = TestContext::new().await?;
    let cookie = login(&contexto).await?;

    for uri in [
        "/admin/devices".to_owned(),
        "/admin/auditoria".to_owned(),
        format!("/admin/vaults/detalle?id={}", contexto.vault_id),
        format!("/admin/vaults/borrar?id={}", contexto.vault_id),
    ] {
        let resp = contexto.request(get_con_cookie(&uri, &cookie)?).await?;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "user en {uri}");
    }

    Ok(())
}

#[tokio::test]
async fn devices_oculta_los_inactivos_y_los_muestra_con_todos() -> Result<()> {
    let contexto = TestContext::new().await?;
    hacer_admin(&contexto).await?;
    let cookie = login(&contexto).await?;

    // Un device que nunca dio señales de vida (ni `last_seen` ni un batch): el vínculo
    // abandonado del caso real. El panel lo oculta por defecto y lo deja auditar con
    // `?todos=1` para poder revocarlo.
    let cuenta: i64 = sqlx::query_scalar("SELECT account_id FROM devices WHERE id = ?")
        .bind(contexto.device_id)
        .fetch_one(&contexto.pool)
        .await?;
    sqlx::query(
        "INSERT INTO devices (account_id, name, platform)
         VALUES (?, 'huerfano-test', 'obsidian')",
    )
    .bind(cuenta)
    .execute(&contexto.pool)
    .await?;

    let resp = contexto
        .request(get_con_cookie("/admin/devices", &cookie)?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = cuerpo(resp).await?;
    assert!(!html.contains("huerfano-test"), "los inactivos quedan ocultos");
    assert!(
        html.contains("inactivos ocultos"),
        "el conteo de ocultos se informa"
    );

    let resp = contexto
        .request(get_con_cookie("/admin/devices?todos=1", &cookie)?)
        .await?;
    assert_eq!(resp.status(), StatusCode::OK);
    let html = cuerpo(resp).await?;
    assert!(
        html.contains("huerfano-test"),
        "con todos=1 aparece para auditarlo"
    );

    Ok(())
}

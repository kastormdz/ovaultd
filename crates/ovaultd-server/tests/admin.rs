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

// ------------------------------------------------------------------ mi cuenta

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
    assert!(cuerpo(response).await?.contains("actualizada"));

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

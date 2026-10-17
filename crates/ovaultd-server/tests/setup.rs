//! Bootstrap por web del primer usuario (`/setup`, ADR-0016): creación de la primera cuenta,
//! cierre automático y token de instalación opcional.
//!
//! Contexto propio y SIN cuentas: el `TestContext` de `common` nace con una cuenta creada
//! y acá justamente hace falta un servidor virgen. Todo lo demás es API pública del crate.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, Request, StatusCode},
};
use http_body_util::BodyExt;
use ovaultd_server::{
    auth,
    config::Config,
    db,
    routes::{self, EstadoServidor},
    storage::CasStore,
};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const PASSWORD: &str = "password-de-prueba-web";

struct Contexto {
    pool: SqlitePool,
    estado: EstadoServidor,
    _db_dir: TempDir,
    _cas_dir: TempDir,
}

impl Contexto {
    async fn new(setup_token: Option<&str>) -> Result<Self> {
        let db_dir = tempfile::tempdir()?;
        let cas_dir = tempfile::tempdir()?;
        let db_path = db_dir.path().join("metadata.db");

        let pool = db::connect(db_path.to_str().ok_or("ruta inválida")?).await?;
        db::migrate(&pool).await?;

        let config = Config {
            bind_addr: "127.0.0.1:0".to_owned(),
            database_path: db_path.to_string_lossy().into_owned(),
            cas_root: cas_dir.path().to_path_buf(),
            token_ttl_hours: 720,
            challenge_ttl_seconds: 300,
            upload_ttl_hours: 24,
            setup_token: setup_token.map(str::to_owned),
            secure_cookies: ovaultd_server::config::ModoSecureCookies::Auto,
        };
        let cas = Arc::new(CasStore::new(cas_dir.path().to_path_buf())?);

        Ok(Self {
            estado: EstadoServidor {
                pool: pool.clone(),
                config,
                cas,
                limitador_login: Default::default(),
            },
            pool,
            _db_dir: db_dir,
            _cas_dir: cas_dir,
        })
    }

    async fn request(&self, request: Request<Body>) -> Result<axum::response::Response> {
        Ok(routes::router(self.estado.clone()).oneshot(request).await?)
    }

    async fn cuentas(&self) -> Result<i64> {
        Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM accounts")
            .fetch_one(&self.pool)
            .await?)
    }

    async fn cuerpo(response: axum::response::Response) -> Result<String> {
        let bytes = response.into_body().collect().await?.to_bytes();
        Ok(String::from_utf8(bytes.to_vec())?)
    }
}

fn post_setup(cuerpo: &str) -> Result<Request<Body>> {
    Ok(Request::post("/setup")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(cuerpo.to_owned()))?)
}

#[tokio::test]
async fn raiz_sin_cuentas_redirige_al_setup() -> Result<()> {
    let contexto = Contexto::new(None).await?;

    let response = contexto
        .request(Request::get("/").body(Body::empty())?)
        .await?;

    assert!(response.status().is_redirection());
    assert_eq!(
        response
            .headers()
            .get("location")
            .and_then(|valor| valor.to_str().ok()),
        Some("/setup")
    );
    Ok(())
}

#[tokio::test]
async fn setup_muestra_el_formulario_sin_cuentas() -> Result<()> {
    let contexto = Contexto::new(None).await?;

    let response = contexto
        .request(Request::get("/setup").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|valor| valor.to_str().ok())
        .is_some_and(|tipo| tipo.starts_with("text/html")));

    let html = Contexto::cuerpo(response).await?;
    assert!(html.contains("name=\"email\""));
    assert!(html.contains("name=\"password\""));
    assert!(html.contains("action=\"/setup\""));
    Ok(())
}

#[tokio::test]
async fn setup_crea_la_primera_cuenta_y_se_cierra() -> Result<()> {
    let contexto = Contexto::new(None).await?;

    let response = contexto
        .request(post_setup(&format!(
            "email=admin@example.com&password={PASSWORD}"
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(Contexto::cuerpo(response).await?.contains("Cuenta creada"));
    assert_eq!(contexto.cuentas().await?, 1);

    // GET y POST quedan cerrados para siempre.
    let response = contexto
        .request(Request::get("/setup").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::GONE);

    let response = contexto
        .request(post_setup(&format!(
            "email=otra@example.com&password={PASSWORD}"
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::GONE);
    assert_eq!(contexto.cuentas().await?, 1);

    // La raíz con cuentas ahora lleva al panel (ADR-0017): login/dashboard.
    let response = contexto
        .request(Request::get("/").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::LOCATION)
            .and_then(|valor| valor.to_str().ok()),
        Some("/admin")
    );
    Ok(())
}

#[tokio::test]
async fn la_cuenta_del_setup_puede_iniciar_sesion() -> Result<()> {
    let contexto = Contexto::new(None).await?;
    contexto
        .request(post_setup(&format!(
            "email=admin@example.com&password={PASSWORD}"
        ))?)
        .await?;

    let response = contexto
        .request(
            Request::post("/api/v1/auth/signin")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "email": "admin@example.com",
                        "password": PASSWORD,
                    })
                    .to_string(),
                ))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let cuerpo: serde_json::Value = serde_json::from_str(&Contexto::cuerpo(response).await?)?;
    assert!(cuerpo["token"].as_str().is_some());
    assert!(cuerpo["device_id"].as_i64().is_some());
    Ok(())
}

#[tokio::test]
async fn setup_con_token_exige_el_token() -> Result<()> {
    let contexto = Contexto::new(Some("token-de-instalacion")).await?;

    let response = contexto
        .request(Request::get("/setup").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = contexto
        .request(Request::get("/setup?token=otro").body(Body::empty())?)
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = contexto
        .request(
            Request::get("/setup?token=token-de-instalacion")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    // El token viaja oculto en el formulario para el POST.
    assert!(Contexto::cuerpo(response).await?.contains("name=\"token\""));

    // POST sin token: rechazado y sin crear nada.
    let response = contexto
        .request(post_setup(&format!(
            "email=admin@example.com&password={PASSWORD}"
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(contexto.cuentas().await?, 0);

    // POST con token: crea.
    let response = contexto
        .request(post_setup(&format!(
            "email=admin@example.com&password={PASSWORD}&token=token-de-instalacion"
        ))?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(contexto.cuentas().await?, 1);
    Ok(())
}

#[tokio::test]
async fn setup_valida_email_y_password() -> Result<()> {
    let contexto = Contexto::new(None).await?;

    let response = contexto
        .request(post_setup("email=sin-arroba&password=password-larga")?)
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = contexto
        .request(post_setup("email=ok@example.com&password=corta")?)
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    assert_eq!(contexto.cuentas().await?, 0);
    Ok(())
}

#[tokio::test]
async fn crear_cuenta_si_vacio_rechaza_la_segunda() -> Result<()> {
    let contexto = Contexto::new(None).await?;

    let primera =
        auth::crear_cuenta_si_vacio(&contexto.pool, "primera@example.com", PASSWORD).await?;
    assert_eq!(primera.email, "primera@example.com");

    let segunda =
        auth::crear_cuenta_si_vacio(&contexto.pool, "segunda@example.com", PASSWORD).await;
    assert!(segunda.is_err());
    assert_eq!(contexto.cuentas().await?, 1);
    Ok(())
}

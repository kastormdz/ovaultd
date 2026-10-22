//! CORS para clientes browser: el runtime del núcleo (Chromium) y el plugin de Obsidian
//! hacen `fetch` cross-origin al server. Sin las cabeceras de CORS el navegador bloquea
//! la respuesta; el upgrade del WebSocket no pasa por CORS y por eso no alcanza con él.

mod common;

use axum::{
    body::Body,
    http::{
        header::{ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD, ORIGIN},
        Method, Request,
    },
};
use common::TestContext;

#[tokio::test]
async fn preflight_del_runtime_browser() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    // Preflight como lo manda Chromium antes del GET autenticado cross-origin.
    let request = Request::builder()
        .method(Method::OPTIONS)
        .uri("/api/v1/vaults/1/changes?since=0")
        .header(ORIGIN, "http://127.0.0.1:18950")
        .header(ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .header(ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
        .body(Body::empty())?;

    let response = context.request(request).await?;
    assert!(
        response.status().is_success(),
        "preflight rechazado: {}",
        response.status()
    );
    let headers = response.headers();
    // Default: SOLO loopback. Se refleja el origen (no `*`), y el navegador puede leer.
    assert_eq!(
        headers
            .get("access-control-allow-origin")
            .and_then(|valor| valor.to_str().ok()),
        Some("http://127.0.0.1:18950"),
    );
    let metodos = headers
        .get("access-control-allow-methods")
        .and_then(|valor| valor.to_str().ok())
        .unwrap_or_default();
    assert!(metodos.contains("GET"), "allow-methods: {metodos}");
    let cabeceras = headers
        .get("access-control-allow-headers")
        .and_then(|valor| valor.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        cabeceras.contains("authorization"),
        "allow-headers: {cabeceras}"
    );

    // Y una respuesta REAL cross-origin también lleva el header (si no, el fetch se bloquea
    // aunque el preflight pase).
    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/vaults")
        .header(ORIGIN, "http://127.0.0.1:18950")
        .header("authorization", format!("Bearer {}", context.token))
        .body(Body::empty())?;
    let response = context.request(request).await?;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|valor| valor.to_str().ok()),
        Some("http://127.0.0.1:18950"),
    );

    Ok(())
}

/// `Origin: localhost` con cualquier puerto: es el mismo caso (runtime en otra máquina).
#[tokio::test]
async fn localhost_tambien_es_local() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let request = Request::builder()
        .method(Method::GET)
        .uri("/healthz")
        .header(ORIGIN, "http://localhost:1234")
        .body(Body::empty())?;
    let response = context.request(request).await?;
    assert_eq!(
        response.headers().get("access-control-allow-origin").and_then(|v| v.to_str().ok()),
        Some("http://localhost:1234"),
    );
    Ok(())
}

/// Una página externa NO puede leer la respuesta: sin `Access-Control-Allow-Origin`.
#[tokio::test]
async fn origen_externo_queda_afuera() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let request = Request::builder()
        .method(Method::GET)
        .uri("/healthz")
        .header(ORIGIN, "https://evil.example")
        .body(Body::empty())?;
    let response = context.request(request).await?;
    assert_eq!(response.status(), 200); // el request se sirve; el navegador no puede LEERLO
    assert!(
        response.headers().get("access-control-allow-origin").is_none(),
        "un origen externo no debe recibir la cabecera de CORS"
    );
    Ok(())
}

/// `OVAULTD_ALLOWED_ORIGINS=*` es el escape hatch documentado.
#[tokio::test]
async fn wildcard_explicito_vuelve_a_permitir_todo() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::con_origenes(vec!["*".to_string()]).await?;
    let request = Request::builder()
        .method(Method::GET)
        .uri("/healthz")
        .header(ORIGIN, "https://evil.example")
        .body(Body::empty())?;
    let response = context.request(request).await?;
    assert_eq!(
        response.headers().get("access-control-allow-origin").and_then(|v| v.to_str().ok()),
        Some("*"),
    );
    Ok(())
}

/// Una lista CSV explícita permite SOLO esos orígenes.
#[tokio::test]
async fn lista_explicita_solo_esos() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::con_origenes(vec!["https://panel.example".to_string()]).await?;
    let permitido = context
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/healthz")
                .header(ORIGIN, "https://panel.example")
                .body(Body::empty())?,
        )
        .await?;
    let ajeno = context
        .request(
            Request::builder()
                .method(Method::GET)
                .uri("/healthz")
                .header(ORIGIN, "https://evil.example")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(
        permitido.headers().get("access-control-allow-origin").and_then(|v| v.to_str().ok()),
        Some("https://panel.example"),
    );
    assert!(ajeno.headers().get("access-control-allow-origin").is_none());
    Ok(())
}

/// Los orígenes del plugin de Obsidian también son legítimos: el core usa `fetch` desde el
/// renderer (`app://obsidian.md`) y desde Capacitor (`capacitor://localhost`). Sin esto, el
/// plugin muere con `TypeError: Failed to fetch` y el log del server queda vacío.
#[tokio::test]
async fn origenes_del_plugin_entran() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    for origen in ["app://obsidian.md", "capacitor://localhost"] {
        let response = context
            .request(
                Request::builder()
                    .method(Method::GET)
                    .uri("/healthz")
                    .header(ORIGIN, origen)
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .and_then(|v| v.to_str().ok()),
            Some(origen),
            "el origen del plugin debe recibir Access-Control-Allow-Origin"
        );
    }
    Ok(())
}

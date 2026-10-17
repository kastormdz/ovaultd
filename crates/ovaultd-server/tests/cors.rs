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
    assert_eq!(
        headers
            .get("access-control-allow-origin")
            .and_then(|valor| valor.to_str().ok()),
        Some("*"),
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
        Some("*"),
    );

    Ok(())
}

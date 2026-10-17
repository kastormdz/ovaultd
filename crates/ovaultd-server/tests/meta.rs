//! Tests de `GET /api/v1/meta` (ADR-0018): el contrato de compatibilidad server ↔ plugin ↔ wire.
//!
//! `/meta` es público a propósito: es lo PRIMERO que un cliente consulta, antes de tener
//! token y antes de vincular un vault. No lleva datos de ninguna cuenta ni de ningún vault.

mod common;

use axum::body::Body;
use axum::http::{
    header::{ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD, ORIGIN},
    Method, Request, StatusCode,
};
use common::{response_json, TestContext};
use serde_json::Value;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn meta(ctx: &TestContext) -> Result<(StatusCode, Value)> {
    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/meta")
        .body(Body::empty())?;
    let response = ctx.request(request).await?;
    let status = response.status();
    Ok((status, response_json(response).await?))
}

/// Extrae `MAJOR.MINOR` de una versión semver: el release train compara solo eso.
fn tren(version: &str) -> String {
    version.split('.').take(2).collect::<Vec<_>>().join(".")
}

#[tokio::test]
async fn meta_es_publico_y_completo() -> Result<()> {
    let context = TestContext::new().await?;

    // El contexto tiene token y cuenta creados: NO se usan. Si `/meta` pidiera auth,
    // este request sin credenciales fallaría con 401.
    let (status, cuerpo) = meta(&context).await?;

    assert_eq!(status, StatusCode::OK, "meta debería ser pública");
    assert_eq!(
        cuerpo["server_version"],
        env!("CARGO_PKG_VERSION"),
        "la versión del server sale del crate, no de una copia a mano"
    );
    assert_eq!(cuerpo["api"], "v1");
    assert_eq!(cuerpo["wire_protocol"], "v2");
    assert!(
        cuerpo["min_plugin_version"].is_string(),
        "min_plugin_version tiene que estar declarada"
    );

    let features = cuerpo["features"]
        .as_array()
        .ok_or("features tiene que ser un array")?;
    assert!(!features.is_empty());
    assert!(
        features.iter().any(|f| f == "meta"),
        "el propio meta tiene que anunciarse"
    );
    Ok(())
}

#[tokio::test]
async fn meta_cumple_el_release_train() -> Result<()> {
    let context = TestContext::new().await?;
    let (_, cuerpo) = meta(&context).await?;

    let server = cuerpo["server_version"].as_str().ok_or("sin versión")?;
    let minimo_plugin = cuerpo["min_plugin_version"].as_str().ok_or("sin mínimo")?;

    // ADR-0018: mismo MAJOR.MINOR = pareja probada junta. Y el mínimo no puede pedir
    // un plugin MÁS NUEVO que el server mismo (sería una pareja imposible de armar).
    assert_eq!(
        tren(server),
        tren(minimo_plugin),
        "server y min_plugin_version tienen que viajar en el mismo tren (MAJOR.MINOR)"
    );
    Ok(())
}

#[tokio::test]
async fn preflight_de_meta_permite_el_header_del_cliente() -> Result<()> {
    let context = TestContext::new().await?;

    // Como lo manda Chromium: el plugin identifica su versión por header (ADR-0018) y
    // ese header custom obliga a un preflight. Si el server no lo permite, el plugin
    // rebota acá y nunca llega a vincular.
    let request = Request::builder()
        .method(Method::OPTIONS)
        .uri("/api/v1/meta")
        .header(ORIGIN, "app://obsidian.md")
        .header(ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .header(ACCESS_CONTROL_REQUEST_HEADERS, "x-ovaultd-client")
        .body(Body::empty())?;

    let response = context.request(request).await?;
    assert!(
        response.status().is_success(),
        "preflight rechazado: {}",
        response.status()
    );

    let permitidos = response
        .headers()
        .get("access-control-allow-headers")
        .and_then(|valor| valor.to_str().ok())
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        permitidos.contains("x-ovaultd-client"),
        "el preflight no permite x-ovaultd-client: {permitidos}"
    );
    Ok(())
}

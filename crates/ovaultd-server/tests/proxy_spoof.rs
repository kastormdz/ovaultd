//! El fix de la IP y del esquema (F-01/F-02 de la auditoría 2026-10-18): SIN `TRUST_PROXY`,
//! los headers de un proxy no cuentan. Rotar `X-Forwarded-For` no esquiva el freno y
//! `X-Forwarded-Proto` no regala el `Secure` de la cookie.

mod common;

use axum::{
    body::Body,
    http::{
        header::{self, CONTENT_TYPE},
        Method, Request, StatusCode,
    },
};

use common::{TestContext, EMAIL, PASSWORD};

fn signin_con_xff(email: &str, xff: &str) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri("/api/v1/auth/signin")
        .header(CONTENT_TYPE, "application/json")
        .header("x-forwarded-for", xff)
        .body(Body::from(
            serde_json::json!({ "email": email, "password": "password-mala-de-prueba" })
                .to_string(),
        ))
        .expect("request válido")
}

#[tokio::test]
async fn rotar_xff_no_esquiva_el_freno_sin_trust_proxy() {
    let mut contexto = TestContext::new().await.unwrap();
    // El modelo "sin borde": los headers de cualquiera NO valen.
    contexto.config.trust_proxy = false;

    // Seis intentos, cada uno con una XFF distinta: si los headers contaran, cada intento
    // sería una clave nueva y el freno jamás dispararía (el PoC de la auditoría).
    for intento in 1..=5 {
        let xff = format!("198.51.100.{intento}");
        let respuesta = contexto.request(signin_con_xff(EMAIL, &xff)).await.unwrap();
        assert_eq!(
            respuesta.status(),
            StatusCode::UNAUTHORIZED,
            "intento {intento}"
        );
    }

    let respuesta = contexto
        .request(signin_con_xff(EMAIL, "198.51.100.6"))
        .await
        .unwrap();
    assert_eq!(
        respuesta.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "el XFF rotado no debe esquivar el freno"
    );
}

#[tokio::test]
async fn xfp_spoofeado_no_regala_el_secure_sin_trust_proxy() {
    let mut contexto = TestContext::new().await.unwrap();
    contexto.config.trust_proxy = false;

    // Login del panel con XFP=https FALSO: sin borde declarado, el esquema no se cree.
    let request = Request::builder()
        .method(Method::POST)
        .uri("/admin/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header("x-forwarded-proto", "https")
        .body(Body::from(format!("email={EMAIL}&password={PASSWORD}")))
        .expect("request válido");

    let respuesta = contexto.request(request).await.unwrap();
    assert_eq!(respuesta.status(), StatusCode::SEE_OTHER);
    let set_cookie = respuesta
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|valor| valor.to_str().ok())
        .expect("sin set-cookie");
    assert!(
        !set_cookie.contains("Secure"),
        "XFP spoofeado no debe regalar el Secure: {set_cookie}"
    );
}

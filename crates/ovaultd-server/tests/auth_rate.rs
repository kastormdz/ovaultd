//! Freno de fuerza bruta en el API de signin (F-01 de la auditoría 2026-10-18): mismo
//! limitador que el panel (5/min por `ip|email`), 429 + `Retry-After`, y la ventana corta
//! también a la password correcta (si no, el freno no frena).

mod common;

use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, header::RETRY_AFTER, Request, StatusCode},
};

use common::{response_json, TestContext, EMAIL, PASSWORD};

fn signin(email: &str, password: &str) -> Request<Body> {
    Request::post("/api/v1/auth/signin")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({ "email": email, "password": password }).to_string(),
        ))
        .expect("request válido")
}

#[tokio::test]
async fn el_sexto_intento_se_frena_con_429_y_retry_after() {
    let contexto = TestContext::new().await.unwrap();

    // Cinco intentos con password mala: todos 401 (fallan contra la cuenta real).
    for intento in 1..=5 {
        let respuesta = contexto
            .request(signin(EMAIL, "password-mala-de-prueba"))
            .await
            .unwrap();
        assert_eq!(respuesta.status(), StatusCode::UNAUTHORIZED, "intento {intento}");
    }

    // El sexto: frenado, con Retry-After y el formato de error del protocolo.
    let respuesta = contexto
        .request(signin(EMAIL, "password-mala-de-prueba"))
        .await
        .unwrap();
    assert_eq!(respuesta.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        respuesta
            .headers()
            .get(RETRY_AFTER)
            .and_then(|valor| valor.to_str().ok()),
        Some("60")
    );
    let cuerpo = response_json(respuesta).await.unwrap();
    assert_eq!(cuerpo["t"], "error");

    // Ni siquiera la password CORRECTA pasa durante la ventana.
    let respuesta = contexto.request(signin(EMAIL, PASSWORD)).await.unwrap();
    assert_eq!(respuesta.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn un_login_exitoso_limpia_la_ventana() {
    let contexto = TestContext::new().await.unwrap();

    // Cuatro fallos (bajo el tope)…
    for _ in 0..4 {
        let respuesta = contexto
            .request(signin(EMAIL, "password-mala-de-prueba"))
            .await
            .unwrap();
        assert_eq!(respuesta.status(), StatusCode::UNAUTHORIZED);
    }

    // …un éxito limpia la ventana…
    let respuesta = contexto.request(signin(EMAIL, PASSWORD)).await.unwrap();
    assert_eq!(respuesta.status(), StatusCode::OK);

    // …y la ventana arranca de cero: cinco fallos más entran, el sexto frena.
    for intento in 1..=5 {
        let respuesta = contexto
            .request(signin(EMAIL, "password-mala-de-prueba"))
            .await
            .unwrap();
        assert_eq!(
            respuesta.status(),
            StatusCode::UNAUTHORIZED,
            "post-limpieza {intento}"
        );
    }
    let respuesta = contexto
        .request(signin(EMAIL, "password-mala-de-prueba"))
        .await
        .unwrap();
    assert_eq!(respuesta.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn el_freno_es_por_email_y_no_global() {
    let contexto = TestContext::new().await.unwrap();

    // Cinco fallos contra una cuenta que NO existe (clave "-|otra@example.com"): el
    // señuelo responde 401 pero el intento CUENTA (es el caso del spraying)…
    for intento in 1..=5 {
        let respuesta = contexto
            .request(signin("otra@example.com", "password-mala-de-prueba"))
            .await
            .unwrap();
        assert_eq!(
            respuesta.status(),
            StatusCode::UNAUTHORIZED,
            "otra {intento}"
        );
    }

    // …el sexto de ESA clave se frena…
    let respuesta = contexto
        .request(signin("otra@example.com", "password-mala-de-prueba"))
        .await
        .unwrap();
    assert_eq!(respuesta.status(), StatusCode::TOO_MANY_REQUESTS);

    // …y la cuenta real no se ve afectada: su ventana está intacta.
    let respuesta = contexto.request(signin(EMAIL, PASSWORD)).await.unwrap();
    assert_eq!(respuesta.status(), StatusCode::OK);
}

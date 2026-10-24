//! Panel web de administración (ADR-0017).
//!
//! Server-rendered: askama compila los templates dentro del binario y htmx viaja
//! vendorizado en `assets/` (sin CDNs, sin build de Node). El panel funciona igual
//! sin JavaScript: los formularios son POST clásicos.
//!
//! Acá viven las piezas COMPARTIDAS entre los módulos de secciones (cuentas, vaults,
//! devices, auditoría): el extractor [`SesionAdmin`], el encoding de mensajes flash y
//! la página de «sin permiso».

pub mod auditoria;
pub mod cuentas;
pub mod devices;
pub mod hora;
pub mod panel;
pub mod sesiones;
pub mod textos;
pub mod vaults;

pub use panel::rutas;

use askama::Template;
use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::routes::EstadoServidor;
use textos::Textos;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

// ------------------------------------------------------------------ piezas compartidas

/// Sesión válida con rol `admin`. Sin rol: página 403 (no redirect: la sesión vale,
/// el permiso no).
pub struct SesionAdmin(pub sesiones::SesionActiva);

impl FromRequestParts<EstadoServidor> for SesionAdmin {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        estado: &EstadoServidor,
    ) -> std::result::Result<Self, Self::Rejection> {
        let sesion = panel::Sesion::from_request_parts(parts, estado)
            .await
            .map_err(IntoResponse::into_response)?;

        if sesion.0.role != "admin" {
            let pagina = panel::render(
                || {
                    PlantillaSinPermiso {
                        t: textos::t(),
                        seccion: "permiso",
                        seccion_titulo: textos::t().sin_permiso_titulo,
                        version: VERSION,
                        email: sesion.0.email.clone(),
                        rol: sesion.0.role.clone(),
                        csrf: sesion.0.csrf_token.clone(),
                    }
                    .render()
                },
                "sin_permiso",
            );
            return Err((StatusCode::FORBIDDEN, pagina).into_response());
        }

        Ok(SesionAdmin(sesion.0))
    }
}

#[derive(Template)]
#[template(path = "admin/sin_permiso.html")]
struct PlantillaSinPermiso {
    t: &'static Textos,
    seccion: &'static str,
    seccion_titulo: &'static str,
    version: &'static str,
    email: String,
    rol: String,
    csrf: String,
}

/// Percent-encoding mínimo para mandar un mensaje humano en un querystring
/// (los acentos y las comillas rompen `HeaderValue`).
pub(crate) fn flash(mensaje: &str) -> String {
    let mut salida = String::with_capacity(mensaje.len() * 3);
    for byte in mensaje.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' => {
                salida.push(byte as char);
            }
            _ => salida.push_str(&format!("%{byte:02X}")),
        }
    }
    salida
}

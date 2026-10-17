//! Panel web de administración (ADR-0017).
//!
//! Server-rendered: askama compila los templates dentro del binario y htmx viaja
//! vendorizado en `assets/` (sin CDNs, sin build de Node). El panel funciona igual
//! sin JavaScript: los formularios son POST clásicos.

pub mod panel;
pub mod sesiones;

pub use panel::rutas;

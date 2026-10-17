//! Puntos de falla inyectables para las pruebas de crash (fase 2, paso 6).
//!
//! REGLA: existen SOLO en builds de debug (`debug_assertions`); un binario release — el
//! del contenedor — los compila a no-op. Inyectar fallas en producción no es una
//! capacidad, es un riesgo; y los `#[cfg(test)]` normales no alcanzan porque los tests
//! de integración compilan el crate SIN `cfg(test)`.
//!
//! Se disparan con `OVAULTD_FAILPOINT=<punto>` (ausente/vacío = nunca) y el modo lo elige
//! `OVAULTD_FAILPOINT_MODE`:
//!   - `abort` (default): `std::process::abort()` — crash real sin unwind (SIGABRT), como
//!     un corte del proceso. Es el modo del test de proceso real.
//!   - `panic`: desenrolla — para los tests in-process que capturan el unwind con
//!     `catch_unwind` y siguen viviendo en el mismo proceso.
//!
//! Los cinco puntos del plan: después de escribir el temporal (`cas.temp`), después del
//! fsync (`cas.fsync`), después de publicar en el CAS (`cas.publish`), después del commit
//! SQL (`sql.commit`) y antes del ACK (`ack.put_blob`). El "antes de ACK" del WebSocket
//! (`sql.commit` es también el último estado durable antes de que el WS responda) no
//! agrega un punto aparte: un punto que no se puede testear sin volver flaky a los demás
//! tests del WS no se agrega "por completitud".

/// Dispara el failpoint si el proceso tiene ese punto activo. No-op en release.
pub fn failpoint(punto: &str) {
    #[cfg(debug_assertions)]
    {
        let activo = std::env::var("OVAULTD_FAILPOINT").unwrap_or_default();
        if activo != punto {
            return;
        }

        let modo = std::env::var("OVAULTD_FAILPOINT_MODE").unwrap_or_else(|_| "abort".to_owned());
        tracing::warn!(punto, modo = %modo, "failpoint disparado (build de debug)");

        if modo == "panic" {
            panic!("failpoint `{punto}` (modo panic)");
        }

        std::process::abort();
    }

    #[cfg(not(debug_assertions))]
    {
        let _ = punto;
    }
}

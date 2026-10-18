//! Mantenimiento operacional del servidor (fase 5 del plan): hoy, el GC de blobs
//! huérfanos. Vive separado de `sync`/`routes` porque son operaciones de OPERADOR:
//! corren desde `ovaultd-admin`, no desde el camino caliente del servicio.

pub mod gc;

//! `ovaultd-server` — servidor del protocolo v2 (documentos cifrados, CAS, auth firmada).
//!
//! Contrato de interfaces: `docs/server-interfaces.md`. Los nombres y las firmas de este archivo y de
//! los módulos están CONGELADOS: cada módulo se implementa por separado contra ellos.
//!
//! Regla de oro: este crate **nunca** recibe `vault_key`, KEK ni recovery, y **nunca** descifra un
//! `path_enc` ni un blob. Lo único que hace con criptografía es verificar: `ovaultd_crypto::verify_record`.

pub mod admin;
pub mod auth;
pub mod config;
pub mod db;
pub mod delta;
pub mod error;
pub mod failpoints;
pub mod history;
pub mod peer;
pub mod routes;
pub mod storage;
pub mod sync;
pub mod uploads;
pub mod ws;

use std::sync::Arc;

use sqlx::SqlitePool;

pub use config::Config;
pub use error::{Result, ServerError};

/// Estado compartido por todos los handlers.
#[derive(Clone)]
pub struct AppState {
    /// Pool de SQLite (WAL, foreign keys, busy_timeout).
    pub pool: SqlitePool,
    /// Almacén de blobs direccionado por contenido.
    pub cas: Arc<storage::CasStore>,
    /// Configuración del proceso.
    pub cfg: Arc<Config>,
    /// Freno de intentos del login del panel (ADR-0017): en memoria, single-node por diseño.
    pub limitador_login: Arc<admin::sesiones::LimitadorDeLogin>,
}

use std::{
    path::Path,
    str::FromStr,
    time::Duration,
};

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};

use crate::error::{Result, ServerError};

/// Convierte una ruta local en una URL `sqlite:` válida.
///
/// Se conservan únicamente los separadores `/` y los caracteres sin reserva
/// de RFC 3986. El resto se escapa como `%XX`, incluyendo `%`, `?`, `#` y
/// espacios, para que no se interpreten como parte de la URL.
fn encode_sqlite_path(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut encoded = String::with_capacity(path.len());

    for byte in path.as_bytes() {
        let is_unreserved = byte.is_ascii_alphanumeric()
            || matches!(*byte, b'-' | b'.' | b'_' | b'~');

        if is_unreserved || *byte == b'/' {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }

    format!("sqlite://{encoded}")
}

pub async fn connect(path: &str) -> Result<SqlitePool> {
    let database_path = Path::new(path);

    // `create_if_missing` crea el archivo, pero SQLite no crea sus directorios
    // padre. Los creamos explícitamente para que una ruta como
    // `./.tmp/server/metadata.db` funcione aunque `.tmp/server` no exista.
    if let Some(parent) = database_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| {
                ServerError::Invalid(format!(
                    "no se pudo crear el directorio padre de la base SQLite \
                     '{path}': {error}"
                ))
            })?;
        }
    }

    let url = encode_sqlite_path(path);
    let options = SqliteConnectOptions::from_str(&url)
        .map_err(|error| {
            ServerError::Invalid(format!(
                "ruta inválida para la base SQLite '{path}': {error}"
            ))
        })?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));

    SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .map_err(|error| {
            ServerError::Invalid(format!(
                "no se pudo abrir la base SQLite '{path}': {error}"
            ))
        })
}

pub async fn migrate(pool: &SqlitePool) -> Result<()> {
    sqlx::migrate!("../../migrations")
        .run(pool)
        .await
        .map_err(|error| {
            ServerError::Invalid(format!(
                "error al ejecutar las migraciones de SQLite: {error}"
            ))
        })
}

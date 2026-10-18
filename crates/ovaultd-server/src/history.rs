//! Historia por archivo y snapshots etiquetados (fase 5, paso 1: exponer sin descifrar).
//!
//! La historia NO necesita tablas nuevas: cada cambio de cada path ya vive en `revisions`
//! (con `idx_rev_path`), firmado, con su blob en el CAS. Acá se EXPONE ese oplog sin
//! descifrar nada — `path_id`/`blob_hash` en hex, `path_enc`/`sig`/`t_mac` en base64 —
//! y el cliente (que tiene las subclaves) es el único que puede leerlo.
//!
//! Los renames se resuelven por metadata histórica: un `move` registra `path_id` (nuevo)
//! y `prev_path_id` (viejo), así que la historia completa de "un archivo" es la unión
//! transitiva de sus paths pasados (CTE recursivo acotado).
//!
//! Los snapshots etiquetan un corte coherente en `rev` y su manifest CUENTA como
//! referencia viva de blobs (invariante de `blobs.refs`, ver `docs/server-interfaces.md` §247):
//! el alta y el borrado ajustan `refs` en la MISMA transacción, que es lo que el GC de la
//! fase 5 va a respetar.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;
use sqlx::{Row, SqlitePool};
use std::collections::BTreeMap;

use crate::error::{Result, ServerError};

/// Tope de saltos de rename que se siguen al armar la cadena de un archivo. Una cadena
/// humana real no llega ni cerca; el tope evita que un grafo patológico convierta la
/// consulta recursiva en un recorrido sin fin.
const MAX_RENAMES: i64 = 32;

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn a_hex32(value: &[u8]) -> Option<String> {
    <[u8; 32]>::try_from(value)
        .ok()
        .map(|bytes| hex_lower(&bytes))
}

fn a_b64(value: &[u8]) -> String {
    STANDARD.encode(value)
}

fn b64_opcional(value: Option<Vec<u8>>) -> Option<String> {
    value.map(|bytes| a_b64(&bytes))
}

fn hex32_opcional(value: Option<Vec<u8>>) -> Option<String> {
    value.and_then(|bytes| a_hex32(&bytes))
}

#[derive(Debug, Serialize)]
pub struct ItemHistorial {
    pub rev: i64,
    pub op: String,
    /// HMAC del path (hex): el nombre en claro lo tiene el cliente, no este lado.
    pub path_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_enc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_path_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t_mac: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    pub device_id: Option<i64>,
    pub epoch: i64,
    pub ts: String,
}

#[derive(Debug, Serialize)]
pub struct Historial {
    pub items: Vec<ItemHistorial>,
    /// `rev` con la que seguir paginando hacia atrás (parámetro `before`). `None` = no hay más.
    pub next_before: Option<i64>,
    /// Piso de retención del vault: por debajo de esto la historia ya no está (se podó).
    pub min_rev: i64,
}

/// Lista la historia de un archivo (por `path_id`), más nueva primero, siguiendo la cadena
/// de renames hacia atrás. `before` acota (`rev < before`); sin renames devuelve lo del
/// path actual solamente.
pub async fn listar(
    pool: &SqlitePool,
    vault_id: i64,
    path_id: &[u8],
    before: Option<i64>,
    limit: i64,
) -> Result<Historial> {
    let min_rev: i64 = sqlx::query_scalar(
        "SELECT min_rev
         FROM vaults
         WHERE id = ?",
    )
    .bind(vault_id)
    .fetch_optional(pool)
    .await?
    .ok_or(ServerError::NotFound)?;

    let before = before.unwrap_or(i64::MAX);

    // La cadena de paths: el pedido + todo `prev_path_id` alcanzable por moves.
    // El LIMIT pide una fila de más: si sobra, hay página siguiente.
    let filas = sqlx::query(
        "WITH RECURSIVE cadena(path_id, saltos) AS (
             SELECT ?, 0
             UNION
             SELECT r.prev_path_id, c.saltos + 1
             FROM revisions r
             JOIN cadena c ON r.path_id = c.path_id
             WHERE r.op = 'move' AND r.prev_path_id IS NOT NULL AND c.saltos < ?
         )
         SELECT rev, op, path_id, path_enc, prev_path_id, blob_hash, sig, t_mac,
                size, mtime, device_id, epoch, ts
         FROM revisions
         WHERE vault_id = ? AND path_id IN (SELECT path_id FROM cadena) AND rev < ?
         ORDER BY rev DESC, path_id
         LIMIT ?",
    )
    .bind(path_id)
    .bind(MAX_RENAMES)
    .bind(vault_id)
    .bind(before)
    .bind(limit + 1)
    .fetch_all(pool)
    .await?;

    let mut items = Vec::with_capacity(filas.len().min(limit as usize + 1));
    for fila in &filas {
        let path_id_bytes: Vec<u8> = fila.get("path_id");
        let Some(path_id_hex) = a_hex32(&path_id_bytes) else {
            continue;
        };

        items.push(ItemHistorial {
            rev: fila.get("rev"),
            op: fila.get("op"),
            path_id: path_id_hex,
            path_enc: b64_opcional(fila.get("path_enc")),
            prev_path_id: hex32_opcional(fila.get("prev_path_id")),
            blob_hash: hex32_opcional(fila.get("blob_hash")),
            sig: b64_opcional(fila.get("sig")),
            t_mac: b64_opcional(fila.get("t_mac")),
            size: fila.get("size"),
            mtime: fila.get("mtime"),
            device_id: fila.get("device_id"),
            epoch: fila.get("epoch"),
            ts: fila.get("ts"),
        });
    }

    let next_before = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        items.last().map(|item| item.rev)
    } else {
        None
    };

    Ok(Historial {
        items,
        next_before,
        min_rev,
    })
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub id: i64,
    pub rev: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: String,
    /// `{path_id hex: blob_id hex}`: qué blobs estaban vivos en `rev`. Cuenta como
    /// referencia viva para el GC (mismo invariante que `files` y `revisions`).
    pub manifest: BTreeMap<String, String>,
}

fn snapshot_desde_fila(fila: &sqlx::sqlite::SqliteRow) -> Result<Snapshot> {
    let manifest_json: String = fila.get("manifest");
    let manifest: BTreeMap<String, String> =
        serde_json::from_str(&manifest_json).map_err(|error| {
            ServerError::Invalid(format!("manifest inválido en snapshots: {error}"))
        })?;

    Ok(Snapshot {
        id: fila.get("id"),
        rev: fila.get("rev"),
        label: fila.get("label"),
        created_at: fila.get("created_at"),
        manifest,
    })
}

/// Crea un snapshot etiquetado sobre un corte coherente en `rev` (default: el head,
/// clampeado al head si piden uno futuro). El manifest sale del índice (`files` vivos
/// a esa revisión) y cada blob suma una referencia en la MISMA transacción.
pub async fn crear_snapshot(
    pool: &SqlitePool,
    vault_id: i64,
    rev: Option<i64>,
    label: Option<String>,
) -> Result<Snapshot> {
    let head_rev: i64 = sqlx::query_scalar(
        "SELECT head_rev
         FROM vaults
         WHERE id = ?",
    )
    .bind(vault_id)
    .fetch_optional(pool)
    .await?
    .ok_or(ServerError::NotFound)?;

    let rev = rev.unwrap_or(head_rev).clamp(0, head_rev);

    let mut tx = pool.begin().await?;

    let filas = sqlx::query(
        "SELECT path_id, blob_hash
         FROM files
         WHERE vault_id = ? AND rev <= ? AND deleted = 0 AND is_folder = 0 AND blob_hash IS NOT NULL",
    )
    .bind(vault_id)
    .bind(rev)
    .fetch_all(&mut *tx)
    .await?;

    let mut manifest = BTreeMap::new();
    for fila in &filas {
        let path_id: Vec<u8> = fila.get("path_id");
        let blob_hash: Vec<u8> = fila.get("blob_hash");
        if let (Some(path_id_hex), Some(blob_hex)) = (a_hex32(&path_id), a_hex32(&blob_hash)) {
            manifest.insert(path_id_hex, blob_hex);
        }
    }

    let manifest_json = serde_json::to_string(&manifest)
        .map_err(|error| ServerError::Invalid(format!("manifest no serializable: {error}")))?;

    let fila = sqlx::query(
        "INSERT INTO snapshots (vault_id, rev, label, manifest)
         VALUES (?, ?, ?, ?)
         RETURNING id, rev, label, created_at, manifest",
    )
    .bind(vault_id)
    .bind(rev)
    .bind(label.as_deref())
    .bind(&manifest_json)
    .fetch_one(&mut *tx)
    .await?;

    for blob_hex in manifest.values() {
        if let Some(bytes) = decodificar_hex32(blob_hex) {
            sqlx::query(
                "UPDATE blobs
                 SET refs = refs + 1
                 WHERE vault_id = ? AND hash = ?",
            )
            .bind(vault_id)
            .bind(&bytes[..])
            .execute(&mut *tx)
            .await?;
        }
    }

    tx.commit().await?;

    snapshot_desde_fila(&fila)
}

fn decodificar_hex32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }

    let mut bytes = [0u8; 32];
    for index in 0..32 {
        bytes[index] = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }

    Some(bytes)
}

pub async fn listar_snapshots(pool: &SqlitePool, vault_id: i64) -> Result<Vec<Snapshot>> {
    let filas = sqlx::query(
        "SELECT id, rev, label, created_at, manifest
         FROM snapshots
         WHERE vault_id = ?
         ORDER BY id DESC",
    )
    .bind(vault_id)
    .fetch_all(pool)
    .await?;

    filas.iter().map(snapshot_desde_fila).collect()
}

pub async fn obtener_snapshot(pool: &SqlitePool, vault_id: i64, id: i64) -> Result<Snapshot> {
    let fila = sqlx::query(
        "SELECT id, rev, label, created_at, manifest
         FROM snapshots
         WHERE vault_id = ? AND id = ?",
    )
    .bind(vault_id)
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(ServerError::NotFound)?;

    snapshot_desde_fila(&fila)
}

/// Borra un snapshot y DEVUELVE sus referencias (decrementa `refs` de cada blob del
/// manifest). Sin esto, un snapshot sería inmortal y el GC nunca podría reclamar su
/// espacio. Devuelve `false` si el snapshot no existía en ese vault.
pub async fn borrar_snapshot(pool: &SqlitePool, vault_id: i64, id: i64) -> Result<bool> {
    let mut tx = pool.begin().await?;

    let fila = sqlx::query(
        "SELECT manifest
         FROM snapshots
         WHERE vault_id = ? AND id = ?",
    )
    .bind(vault_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(fila) = fila else {
        return Ok(false);
    };

    let manifest_json: String = fila.get("manifest");
    let manifest: BTreeMap<String, String> =
        serde_json::from_str(&manifest_json).map_err(|error| {
            ServerError::Invalid(format!("manifest inválido en snapshots: {error}"))
        })?;

    for blob_hex in manifest.values() {
        if let Some(bytes) = decodificar_hex32(blob_hex) {
            // `AND refs > 0` es defensivo: un contador negativo enmascararía un bug de
            // contabilidad en vez de hacerlo visible en el próximo GC.
            sqlx::query(
                "UPDATE blobs
                 SET refs = refs - 1
                 WHERE vault_id = ? AND hash = ? AND refs > 0",
            )
            .bind(vault_id)
            .bind(&bytes[..])
            .execute(&mut *tx)
            .await?;
        }
    }

    sqlx::query(
        "DELETE FROM snapshots
         WHERE vault_id = ? AND id = ?",
    )
    .bind(vault_id)
    .bind(id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(true)
}

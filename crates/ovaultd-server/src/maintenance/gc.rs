//! GC de blobs huérfanos (fase 5, paso 5 del plan).
//!
//! El invariante de `blobs.refs` lo mantienen DOS caminos y solo dos: el commit suma 1
//! por operación con blob (que es, a la vez, la revisión del path — `files` es su
//! proyección viva, no una fuente independiente) y los snapshots suman/restan 1 por
//! cada blob de su manifest. Un blob con `refs = 0` es, entonces, un HUÉRFANO: se
//! publicó (upload o `blob_head`) pero ningún batch lo referenció, o su única referencia
//! era un snapshot que se borró.
//!
//! Modos:
//!
//! - `planificar` (dry-run, el default): lista los candidatos — `refs = 0` con la
//!   gracia vencida — sin escribir NADA.
//! - `verificar`: audita el invariante recontando las fuentes (revisiones con blob +
//!   manifests de snapshots) y comparándolas contra `blobs.refs` en AMBOS sentidos
//!   (drift = bug en un camino que ajusta referencias; referencia colgada = una fuente
//!   que apunta a un blob sin fila).
//! - `borrar`: destructivo, en UNA transacción, y excluye los blobs de uploads en
//!   curso: un blob recién publicado todavía no tiene su batch (su `refs` es 0 hasta el
//!   commit) y la gracia es la red que lo protege.

use std::collections::{BTreeMap, BTreeSet};

use sqlx::SqlitePool;

use crate::error::{Result, ServerError};
use crate::storage::CasStore;
use ovaultd_protocol::BlobId;

/// Gracia por defecto: 24 h. Cubre el hueco entre publicar un blob y commitear su batch
/// (segundos), con margen para reintentos y relojes desfasados.
pub const GRACIA_MINUTOS_POR_DEFECTO: i64 = 24 * 60;

/// Un blob sin referencias vivas y con la gracia vencida: candidato a borrado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidato {
    pub vault_id: i64,
    pub hash: Vec<u8>,
    pub size: i64,
    pub created_at: String,
}

impl Candidato {
    /// Hex en minúsculas del blob_id, como lo muestra el reporte.
    pub fn hash_hex(&self) -> String {
        self.hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// Plan (dry-run): lo que `borrar` va a borrar. NUNCA escribe.
pub async fn planificar(
    pool: &SqlitePool,
    vault: Option<i64>,
    gracia_minutos: i64,
) -> Result<Vec<Candidato>> {
    let modificador = format!("-{gracia_minutos} minutes");
    let filas = sqlx::query_as::<_, (i64, Vec<u8>, i64, String)>(
        "SELECT vault_id, hash, size, created_at
         FROM blobs
         WHERE refs = 0
           AND (?1 IS NULL OR vault_id = ?1)
           AND created_at <= datetime('now', ?2)
         ORDER BY vault_id, hash",
    )
    .bind(vault)
    .bind(&modificador)
    .fetch_all(pool)
    .await?;

    Ok(filas
        .into_iter()
        .map(|(vault_id, hash, size, created_at)| Candidato {
            vault_id,
            hash,
            size,
            created_at,
        })
        .collect())
}

/// Una diferencia entre `blobs.refs` y el recuento de las fuentes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discrepancia {
    pub vault_id: i64,
    pub hash: Vec<u8>,
    pub refs: i64,
    pub esperado: i64,
}

impl Discrepancia {
    /// Hex en minúsculas del blob_id.
    pub fn hash_hex(&self) -> String {
        self.hash
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// Audita el invariante: `refs` = revisiones con blob + manifests de snapshots, por
/// blob. Devuelve las discrepancias (vacío = sano). NUNCA escribe.
pub async fn verificar(pool: &SqlitePool, vault: Option<i64>) -> Result<Vec<Discrepancia>> {
    let mut esperado: BTreeMap<(i64, Vec<u8>), i64> = BTreeMap::new();

    // (1) Revisiones con blob: `del` y carpetas no llevan (`blob_hash` NULL).
    for (vault_id, hash, n) in sqlx::query_as::<_, (i64, Vec<u8>, i64)>(
        "SELECT vault_id, blob_hash, COUNT(*)
         FROM revisions
         WHERE blob_hash IS NOT NULL AND (?1 IS NULL OR vault_id = ?1)
         GROUP BY vault_id, blob_hash",
    )
    .bind(vault)
    .fetch_all(pool)
    .await?
    {
        *esperado.entry((vault_id, hash)).or_default() += n;
    }

    // (2) Snapshots: el manifest es JSON `{path_id: blob_id}` con blob_id en hex.
    let manifiestos = sqlx::query_as::<_, (i64, String)>(
        "SELECT vault_id, manifest FROM snapshots WHERE ?1 IS NULL OR vault_id = ?1",
    )
    .bind(vault)
    .fetch_all(pool)
    .await?;
    for (vault_id, manifest) in manifiestos {
        let mapa: BTreeMap<String, String> = serde_json::from_str(&manifest).map_err(|error| {
            ServerError::Invalid(format!("manifest inválido en snapshots: {error}"))
        })?;
        for blob_hex in mapa.values() {
            let hash = blob_hex.parse::<BlobId>().map_err(|_| {
                ServerError::Invalid(format!(
                    "blob_id inválido en manifest de snapshot: {blob_hex}"
                ))
            })?;
            *esperado
                .entry((vault_id, hash.as_bytes().to_vec()))
                .or_default() += 1;
        }
    }

    // Comparación en AMBOS sentidos: drift en `refs`, y referencias colgadas (fuentes
    // que apuntan a un blob sin fila en `blobs`).
    let mut discrepancias = Vec::new();
    let mut vistos: BTreeSet<(i64, Vec<u8>)> = BTreeSet::new();
    for (vault_id, hash, refs) in sqlx::query_as::<_, (i64, Vec<u8>, i64)>(
        "SELECT vault_id, hash, refs FROM blobs WHERE ?1 IS NULL OR vault_id = ?1",
    )
    .bind(vault)
    .fetch_all(pool)
    .await?
    {
        let cuenta = esperado
            .get(&(vault_id, hash.clone()))
            .copied()
            .unwrap_or(0);
        if cuenta != refs {
            discrepancias.push(Discrepancia {
                vault_id,
                hash: hash.clone(),
                refs,
                esperado: cuenta,
            });
        }
        vistos.insert((vault_id, hash));
    }
    for ((vault_id, hash), cuenta) in esperado {
        if !vistos.contains(&(vault_id, hash.clone())) {
            discrepancias.push(Discrepancia {
                vault_id,
                hash,
                refs: 0,
                esperado: cuenta,
            });
        }
    }

    Ok(discrepancias)
}

/// Borra de verdad: filas de `blobs` + archivos del CAS. Devuelve (borrados, bytes).
///
/// Re-verifica en la MISMA transacción (`refs = 0`): el plan pudo envejecer entre
/// `planificar` y `borrar`. Excluye los hashes con uploads en curso (`offset < total`):
/// su blob está a medio llegar y un `refs = 0` transitorio no es un huérfano.
///
/// El CAS va DESPUÉS del commit: si el proceso muere entre medio, el archivo queda
/// huérfano en disco (invisible, espacio a recuperar) — al revés, una fila sin blob
/// sería un vault roto. `CasStore::remove` es idempotente: el reintento es seguro.
pub async fn borrar(
    pool: &SqlitePool,
    cas: &CasStore,
    candidatos: &[Candidato],
) -> Result<(usize, i64)> {
    let mut tx = pool.begin().await?;
    let mut a_borrar: Vec<Vec<u8>> = Vec::new();
    let mut bytes = 0i64;

    for candidato in candidatos {
        let en_curso: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM uploads
             WHERE vault_id = ? AND hash = ? AND offset < total",
        )
        .bind(candidato.vault_id)
        .bind(&candidato.hash)
        .fetch_one(&mut *tx)
        .await?;
        if en_curso > 0 {
            continue;
        }

        let resultado = sqlx::query(
            "DELETE FROM blobs
             WHERE vault_id = ? AND hash = ? AND refs = 0",
        )
        .bind(candidato.vault_id)
        .bind(&candidato.hash)
        .execute(&mut *tx)
        .await?;
        if resultado.rows_affected() == 1 {
            bytes += candidato.size;
            a_borrar.push(candidato.hash.clone());
        }
    }

    tx.commit().await?;

    for hash in &a_borrar {
        let bytes_hash: [u8; 32] = hash
            .as_slice()
            .try_into()
            .map_err(|_| ServerError::Invalid("blob_id de largo inválido".to_owned()))?;
        cas.remove(&BlobId::new(bytes_hash)).await?;
    }

    Ok((a_borrar.len(), bytes))
}

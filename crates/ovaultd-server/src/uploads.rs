//! Subidas reanudables (`tus` core) y reservas de cuota — ADR-0012.
//!
//! El wire y las garantías están en `docs/server-interfaces.md` (§ "Subidas reanudables"). Los dos
//! invariantes que este módulo sostiene son los del ADR:
//!
//! 1. **El offset confirmado vive en la base** (sobrevive reinicios) y el `.part` se alinea a él
//!    antes de cada escritura: un fallo de disco no deja una cola de bytes sin confirmar.
//! 2. **La longitud declarada no se cree**: un bloque que pase el `total` termina la subida, y al
//!    completar se verifica el hash REAL del archivo contra el `blob_id` declarado. Completar es
//!    CAS → índice → `offset = total`, en ese orden: cualquier fallo deja la subida reanudable y el
//!    chunk final reintentado es idempotente.
//!
//! La reserva de cuota de una subida **viva** es su `total` completo: los bytes ya recibidos
//! viven en el `.part` y los que faltan están comprometidos — nada de eso es espacio libre. Una
//! subida completa aporta 0 (sus bytes pasan a `blobs`), y por eso el barrido de vencidas
//! (`barrer`) es lo único que hace falta para liberar el espacio de un upload abandonado.

use std::collections::HashMap;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::{Result, ServerError};
use crate::storage::{blob_registrado, registrar_blob, CasStore};
use ovaultd_protocol::{BlobId, ErrorCode};

/// Subidas sin actividad que el barrido ya puede borrar (se configura con
/// `OVAULTD_UPLOAD_TTL_HOURS`; este es el valor documentado por defecto).
pub const TTL_HORAS_DEFAULT: i64 = 24;

/// Lo que devuelve crear una subida (ya con el offset inicial resuelto: dedupe ⇒ nace completa).
#[derive(Debug)]
pub struct Creada {
    pub upload_id: String,
    pub offset: i64,
    pub total: i64,
}

/// Estado persistido de una subida, para `HEAD`.
#[derive(Debug)]
pub struct Estado {
    pub offset: i64,
    pub total: i64,
}

/// Resultado de aplicar un bloque.
#[derive(Debug, PartialEq, Eq)]
pub enum Aplicado {
    /// Quedaron bytes pendientes: nuevo offset confirmado.
    Parcial { offset: i64 },
    /// Se completó: hash verificado, CAS publicado, índice registrado y offset final confirmado.
    Completo { offset: i64 },
    /// Ya estaba completa (reintento del bloque final tras una respuesta perdida): no se
    /// reescribe nada.
    YaCompleto { offset: i64 },
}

/// Los `.part` viven fuera del sharding del CAS (`aa/resto`), en un dotdir.
pub fn directorio(cas_root: &Path) -> PathBuf {
    cas_root.join(".uploads")
}

fn temporal(directorio: &Path, upload_id: &str) -> PathBuf {
    directorio.join(format!("{upload_id}.part"))
}

/// Candado por subida: dos `PATCH` simultáneos del mismo upload se serializan (el append al
/// archivo no es transaccional; el offset de la base evita confirmaciones cruzadas).
///
/// El mapa crece con cada upload_id visto (36 B por entrada): irrelevante para un self-hosted,
/// y no se poda para no arriesgar un candado en uso.
fn candado(upload_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static CANDADOS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();

    let mapa = CANDADOS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guardia = mapa.lock().unwrap_or_else(|envenenado| envenenado.into_inner());

    guardia
        .entry(upload_id.to_owned())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Crea la subida y RESERVA la cuota del total declarado.
///
/// El chequeo y la reserva son UNA sola sentencia (`INSERT … SELECT` con guard): dos creaciones
/// simultáneas no pueden pasar las dos — la segunda ya ve la reserva de la primera. Si el blob
/// ya está registrado para el vault, la subida nace completa (dedupe) y no reserva nada.
pub async fn crear(
    pool: &SqlitePool,
    cas: &crate::storage::CasStore,
    vault_id: i64,
    blob_id: &BlobId,
    total: i64,
    ttl_horas: i64,
) -> Result<Creada> {
    let upload_id = Uuid::new_v4().to_string();

    // Dedupe contra el CAS FÍSICO, no sólo contra la tabla `blobs`: el mismo contenido puede
    // estar subido por otro vault y no tiene sentido reenviarlo entero. Si el archivo ya
    // está, se registra para este vault y la subida nace completa.
    let registrado = blob_registrado(pool, vault_id, blob_id).await?;
    let en_disco = cas.exists(blob_id);
    if !registrado && en_disco {
        crate::storage::registrar_blob(pool, vault_id, blob_id, total).await?;
    }
    let offset_inicial = if registrado || en_disco { total } else { 0 };

    // ?1..?6 densos y en orden: sqlx binda posicional contra el índice máximo de SQLite.
    let insertadas = sqlx::query(
        "INSERT INTO uploads (upload_id, vault_id, hash, total, offset, expires_at)
         SELECT ?1, ?2, ?3, ?4, ?5, datetime('now', ?6)
         WHERE ?5 = ?4
            OR (SELECT quota_bytes FROM vaults WHERE id = ?2) IS NULL
            OR (SELECT quota_bytes FROM vaults WHERE id = ?2)
               - IFNULL((SELECT SUM(size) FROM blobs WHERE vault_id = ?2), 0)
               - IFNULL((
                   SELECT SUM(CASE WHEN offset < total THEN total ELSE 0 END)
                   FROM uploads WHERE vault_id = ?2
                 ), 0)
               >= ?4",
    )
    .bind(&upload_id)
    .bind(vault_id)
    .bind(blob_id.as_bytes().as_slice())
    .bind(total)
    .bind(offset_inicial)
    .bind(format!("+{ttl_horas} hours"))
    .execute(pool)
    .await?
    .rows_affected();

    if insertadas == 0 {
        return Err(ServerError::Protocol(ErrorCode::QuotaExceeded));
    }

    Ok(Creada {
        upload_id,
        offset: offset_inicial,
        total,
    })
}

/// Estado de una subida **vigente** (una vencida es `None`: se comporta como inexistente).
pub async fn estado(pool: &SqlitePool, vault_id: i64, upload_id: &str) -> Result<Option<Estado>> {
    let fila = sqlx::query_as::<_, (i64, i64)>(
        "SELECT offset, total
         FROM uploads
         WHERE upload_id = ? AND vault_id = ? AND expires_at > datetime('now')",
    )
    .bind(upload_id)
    .bind(vault_id)
    .fetch_optional(pool)
    .await?;

    Ok(fila.map(|(offset, total)| Estado { offset, total }))
}

/// Aplica un bloque. Ver el contrato en `docs/server-interfaces.md` y las garantías arriba.
#[allow(clippy::too_many_arguments)] // pool/cas/dir + identidad de la subida + offset/ttl + bytes
pub async fn aplicar(
    pool: &SqlitePool,
    cas: &CasStore,
    cas_root: &Path,
    vault_id: i64,
    upload_id: &str,
    offset_pedido: i64,
    ttl_horas: i64,
    bytes: Vec<u8>,
) -> Result<Aplicado> {
    let candado = candado(upload_id);
    let _guardia = candado.lock().await;

    let (hash, total, offset_actual) = sqlx::query_as::<_, (Vec<u8>, i64, i64)>(
        "SELECT hash, total, offset
         FROM uploads
         WHERE upload_id = ? AND vault_id = ? AND expires_at > datetime('now')",
    )
    .bind(upload_id)
    .bind(vault_id)
    .fetch_optional(pool)
    .await?
    .ok_or(ServerError::NotFound)?;

    if hash.len() != 32 {
        return Err(ServerError::Invalid(
            "el blob_id guardado del upload no tiene 32 bytes".to_owned(),
        ));
    }

    if offset_actual == total {
        return Ok(Aplicado::YaCompleto { offset: total });
    }

    if offset_pedido != offset_actual {
        return Err(ServerError::UploadOffset {
            actual: offset_actual,
            requested: offset_pedido,
        });
    }

    let largo = i64::try_from(bytes.len())
        .map_err(|_| ServerError::Invalid("bloque demasiado grande".to_owned()))?;

    let nuevo = offset_actual
        .checked_add(largo)
        .ok_or_else(|| ServerError::Invalid("la suma de offsets desborda".to_owned()))?;

    // No se cree la longitud declarada: pasarse del total es terminal para ESA subida.
    if nuevo > total {
        descartar(pool, cas_root, upload_id).await;
        return Err(ServerError::Invalid(
            "los bytes exceden la longitud declarada (Upload-Length)".to_owned(),
        ));
    }

    // La base manda: el archivo se alinea al offset confirmado antes de escribir (un reintento
    // tras un fallo de disco no deja cola de bytes sin confirmar).
    let ruta = temporal(&directorio(cas_root), upload_id);
    escribir_bloque(ruta.clone(), offset_actual, bytes).await?;

    let mut hash_bytes = [0u8; 32];
    hash_bytes.copy_from_slice(&hash);
    let blob_id = BlobId::new(hash_bytes);

    if nuevo < total {
        confirmar_offset(pool, upload_id, offset_actual, nuevo, offset_pedido, ttl_horas).await?;
        return Ok(Aplicado::Parcial { offset: nuevo });
    }

    // Completa: el hash REAL del archivo contra el blob_id declarado, ANTES de publicar.
    let ruta_para_leer = ruta.clone();
    let contenido = tokio::task::spawn_blocking(move || std::fs::read(&ruta_para_leer))
        .await
        .map_err(|_| ServerError::Invalid("falló la tarea de lectura".to_owned()))??;

    if Sha256::digest(&contenido).as_slice() != blob_id.as_bytes() {
        descartar(pool, cas_root, upload_id).await;
        return Err(ServerError::Invalid(
            "el hash del contenido no coincide con el blob_id del upload".to_owned(),
        ));
    }

    // Dos fases: temporal → fsync → CAS (idempotente), y recién después el índice y el offset:
    // un fallo a mitad deja la subida reanudable y el chunk final reintentado republica sin
    // duplicar nada.
    cas.publish(&blob_id, &contenido).await?;
    registrar_blob(pool, vault_id, &blob_id, total).await?;
    confirmar_offset(pool, upload_id, offset_actual, total, offset_pedido, ttl_horas).await?;

    // El contenido ya vive en el CAS: el temporal se va (si esto falla, el barrido lo recoge).
    let _ = std::fs::remove_file(&ruta);

    Ok(Aplicado::Completo { offset: total })
}

/// Escribe el bloque en el `.part` alineado al offset confirmado, con `fsync` antes de volver.
async fn escribir_bloque(ruta: PathBuf, offset: i64, bytes: Vec<u8>) -> Result<()> {
    let offset =
        u64::try_from(offset).map_err(|_| ServerError::Invalid("offset negativo".to_owned()))?;

    tokio::task::spawn_blocking(move || -> Result<()> {
        if let Some(padre) = ruta.parent() {
            std::fs::create_dir_all(padre)?;
        }

        let mut archivo = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false) // el largo lo fija `set_len` contra el offset confirmado
            .open(&ruta)?;

        // Si el `.part` quedó MÁS CORTO que el offset confirmado (crash, limpieza, disco
        // nuevo), `set_len` lo extendería con CEROS y el hash final fallaría culpando al
        // cliente. Mejor 409: el cliente reinicia la subida desde cero.
        let largo = archivo.metadata()?.len();
        if largo < offset {
            return Err(ServerError::UploadOffset {
                actual: i64::try_from(offset).unwrap_or(0),
                requested: i64::try_from(largo).unwrap_or(0),
            });
        }

        archivo.set_len(offset)?;
        archivo.seek(SeekFrom::Start(offset))?;
        archivo.write_all(&bytes)?;
        archivo.sync_data()?;

        Ok(())
    })
    .await
    .map_err(|_| ServerError::Invalid("falló la tarea de escritura".to_owned()))?
}

async fn confirmar_offset(
    pool: &SqlitePool,
    upload_id: &str,
    esperado: i64,
    nuevo: i64,
    pedido: i64,
    ttl_horas: i64,
) -> Result<()> {
    // Cada bloque confirmado renueva el TTL (una subida activa no expira a mitad de camino).
    let confirmadas = sqlx::query(
        "UPDATE uploads
         SET offset = ?, expires_at = datetime('now', ?)
         WHERE upload_id = ? AND offset = ?",
    )
    .bind(nuevo)
    .bind(format!("+{ttl_horas} hours"))
    .bind(upload_id)
    .bind(esperado)
    .execute(pool)
    .await?
    .rows_affected();

    if confirmadas != 1 {
        return Err(ServerError::UploadOffset {
            actual: esperado,
            requested: pedido,
        });
    }

    Ok(())
}

/// Termina una subida defectuosa: fila + temporal afuera ("subir de nuevo desde cero", nunca un
/// truncado silencioso). Si algo falla acá, el barrido recoge los restos.
pub async fn descartar(pool: &SqlitePool, cas_root: &Path, upload_id: &str) {
    let _ = sqlx::query("DELETE FROM uploads WHERE upload_id = ?")
        .bind(upload_id)
        .execute(pool)
        .await;

    let _ = std::fs::remove_file(temporal(&directorio(cas_root), upload_id));
}

/// Barrido perezoso (corre al crear una subida):
///
/// 1. Vencidas: fila + temporal afuera — ESTO libera la reserva de cuota.
/// 2. `.part` huérfanos (sin fila) con más de una hora: restos de un crash entre el DELETE y el
///    borrado del archivo. La hora protege al que está naciendo (fila primero, archivo después).
pub async fn barrer(pool: &SqlitePool, cas_root: &Path) -> Result<u64> {
    let directorio = directorio(cas_root);

    let vencidas: Vec<String> = sqlx::query_scalar(
        "DELETE FROM uploads
         WHERE expires_at <= datetime('now')
         RETURNING upload_id",
    )
    .fetch_all(pool)
    .await?;

    let mut barridas = u64::try_from(vencidas.len()).unwrap_or(0);

    for upload_id in &vencidas {
        let _ = std::fs::remove_file(temporal(&directorio, upload_id));
    }

    let Ok(entradas) = std::fs::read_dir(&directorio) else {
        return Ok(barridas);
    };

    for entrada in entradas.flatten() {
        let ruta = entrada.path();

        if ruta.extension().and_then(|extension| extension.to_str()) != Some("part") {
            continue;
        }

        let Some(upload_id) = ruta.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };

        let viejo = match entrada.metadata().and_then(|metadata| metadata.modified()) {
            Ok(modificado) => modificado
                .elapsed()
                .map(|edad| edad.as_secs() >= 3600)
                .unwrap_or(false),
            Err(_) => false,
        };

        if !viejo {
            continue;
        }

        let existe: Option<i64> = sqlx::query_scalar("SELECT 1 FROM uploads WHERE upload_id = ?")
            .bind(upload_id)
            .fetch_optional(pool)
            .await?;

        if existe.is_none() && std::fs::remove_file(&ruta).is_ok() {
            barridas += 1;
        }
    }

    Ok(barridas)
}

use crate::error::{Result, ServerError};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use ovaultd_protocol::BlobId;
use tokio::sync::broadcast;
use tokio::task;
use uuid::Uuid;

pub struct CasStore {
    root: PathBuf,
}

impl CasStore {
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Raíz del CAS (para backups y verificaciones externas).
    pub fn raiz(&self) -> &Path {
        &self.root
    }

    pub async fn publish(&self, hash: &BlobId, bytes: &[u8]) -> Result<()> {
        let root = self.root.clone();
        let hash = *hash;
        let bytes = bytes.to_vec();

        task::spawn_blocking(move || publish_sync(&root, &hash, &bytes))
            .await
            .map_err(|_| ServerError::Invalid("falló la tarea de almacenamiento".to_owned()))?
    }

    pub async fn read(&self, hash: &BlobId) -> Result<Vec<u8>> {
        let path = self.blob_path(hash);
        let hash = *hash;

        task::spawn_blocking(move || {
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(ServerError::NotFound);
                }
                Err(error) => return Err(ServerError::Io(error)),
            };

            let digest = Sha256::digest(&bytes);
            if digest.as_slice() != hash.as_bytes() {
                return Err(ServerError::Invalid(
                    "el contenido del blob no coincide con su hash".to_owned(),
                ));
            }

            Ok(bytes)
        })
        .await
        .map_err(|_| ServerError::Invalid("falló la tarea de almacenamiento".to_owned()))?
    }

    pub fn exists(&self, hash: &BlobId) -> bool {
        self.blob_path(hash).is_file()
    }

    /// Borra el blob del CAS (barrido tras borrar un vault). Idempotente: si ya no está, ok.
    pub async fn remove(&self, hash: &BlobId) -> Result<()> {
        let path = self.blob_path(hash);

        task::spawn_blocking(move || match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ServerError::Io(error)),
        })
        .await
        .map_err(|_| ServerError::Invalid("falló la tarea de almacenamiento".to_owned()))?
    }

    fn blob_path(&self, hash: &BlobId) -> PathBuf {
        let hex = hash.to_string();
        self.root.join(&hex[..2]).join(hex)
    }
}

/// Registra el blob en el índice (`blobs`) **después** de publicarlo en el CAS.
///
/// `commit_batch` rechaza con `not_found` un batch que referencie un `blob_id` sin fila en `blobs`
/// ("no se referencia contenido que no está"): publicar los bytes no alcanza, el índice es la
/// prueba de que el blob existe para el vault. Idempotente (`ON CONFLICT DO NOTHING`): repetir una
/// subida no pisa `refs`, que solo mueve el commit.
pub async fn registrar_blob(
    pool: &SqlitePool,
    vault_id: i64,
    blob_id: &BlobId,
    size: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO blobs (vault_id, hash, size)
         VALUES (?, ?, ?)
         ON CONFLICT(vault_id, hash) DO NOTHING",
    )
    .bind(vault_id)
    .bind(blob_id.as_bytes().as_slice())
    .bind(size)
    .execute(pool)
    .await?;

    Ok(())
}

/// ¿El vault tiene ese blob registrado en el índice?
///
/// Es la **frontera de autorización del CAS compartido**: el CAS guarda bytes por hash para TODO
/// el servidor (un solo root), así que sin esta fila cualquiera con el hash lee el blob de otro
/// vault. La misma consulta la usa el commit (transacción) y la descarga (pool).
pub async fn blob_registrado<'e, E>(executor: E, vault_id: i64, blob_id: &BlobId) -> Result<bool>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let existe = sqlx::query_scalar::<_, i64>(
        "SELECT 1
         FROM blobs
         WHERE vault_id = ? AND hash = ?
         LIMIT 1",
    )
    .bind(vault_id)
    .bind(blob_id.as_bytes().as_slice())
    .fetch_optional(executor)
    .await?;

    Ok(existe.is_some())
}

/// ¿Entra este blob en la cuota **opt-in** del vault? (`quota_bytes`; `NULL` = ilimitado).
///
/// Es el chequeo de los caminos que **no reservan** (PUT y WS inline): cuenta lo registrado
/// (`blobs.size`) + las reservas vivas de las subidas reanudables (el `total` de cada subida
/// que todavía no completó).
/// Un blob ya registrado no consume espacio nuevo (el CAS dedupe por hash y `blobs` tiene una
/// fila por vault): re-subirlo siempre entra. La creación de un upload hace su PROPIA
/// verificación atómica (`INSERT … SELECT` con guard, ADR-0012) — acá no hay reserva, así que
/// dos subidas chicas simultáneas podrían pasarse por poco (ventana aceptada y documentada).
pub async fn espacio_disponible(
    pool: &SqlitePool,
    vault_id: i64,
    blob_id: &BlobId,
    size: i64,
) -> Result<bool> {
    let entra = sqlx::query_scalar::<_, i64>(
        "SELECT CASE
             WHEN EXISTS (
                 SELECT 1 FROM blobs WHERE vault_id = ?1 AND hash = ?2
             ) THEN 1
             WHEN (SELECT quota_bytes FROM vaults WHERE id = ?1) IS NULL THEN 1
             WHEN (SELECT quota_bytes FROM vaults WHERE id = ?1)
                  - IFNULL((SELECT SUM(size) FROM blobs WHERE vault_id = ?1), 0)
                  - IFNULL((
                      SELECT SUM(CASE WHEN offset < total THEN total ELSE 0 END)
                      FROM uploads WHERE vault_id = ?1
                    ), 0)
                  >= ?3
             THEN 1
             ELSE 0
         END",
    )
    .bind(vault_id)
    .bind(blob_id.as_bytes().as_slice())
    .bind(size)
    .fetch_one(pool)
    .await?;

    Ok(entra == 1)
}

fn publish_sync(root: &Path, hash: &BlobId, bytes: &[u8]) -> Result<()> {
    let destination = {
        let hex = hash.to_string();
        root.join(&hex[..2]).join(hex)
    };
    let directory = destination
        .parent()
        .ok_or_else(|| ServerError::Invalid("ruta de almacenamiento inválida".to_owned()))?;

    fs::create_dir_all(directory)?;

    match fs::metadata(&destination) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(ServerError::Invalid(
                    "el destino del blob no es un archivo".to_owned(),
                ));
            }

            if metadata.len() == bytes.len() as u64 {
                return Ok(());
            }

            return Err(ServerError::Invalid(
                "el blob existente tiene un tamaño incorrecto".to_owned(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ServerError::Io(error)),
    }

    let temporary = directory.join(format!(".tmp-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;

        file.write_all(bytes)?;

        let metadata = file.metadata()?;
        if metadata.len() != bytes.len() as u64 {
            return Err(ServerError::Invalid(
                "la longitud escrita no coincide con la esperada".to_owned(),
            ));
        }

        let digest = Sha256::digest(bytes);
        if digest.as_slice() != hash.as_bytes() {
            return Err(ServerError::Invalid(
                "el contenido no coincide con el hash solicitado".to_owned(),
            ));
        }

        // Failpoint de test (no-op en release): crash después de escribir el temporal.
        crate::failpoints::failpoint("cas.temp");

        file.sync_all()?;
        drop(file);

        // Failpoint de test: crash después del fsync, antes de publicar.
        crate::failpoints::failpoint("cas.fsync");

        match fs::rename(&temporary, &destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = fs::metadata(&destination)?;
                if !existing.is_file() {
                    return Err(ServerError::Invalid(
                        "el destino del blob no es un archivo".to_owned(),
                    ));
                }

                if existing.len() != bytes.len() as u64 {
                    return Err(ServerError::Invalid(
                        "el blob existente tiene un tamaño incorrecto".to_owned(),
                    ));
                }

                return Ok(());
            }
            Err(error) => return Err(ServerError::Io(error)),
        }

        File::open(directory)?.sync_all()?;

        // Failpoint de test: crash después de publicar en el CAS (rename + fsync del
        // directorio), antes de que el llamador registre el blob en el índice.
        crate::failpoints::failpoint("cas.publish");

        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }

    result
}

/// Señal de "hay revisiones nuevas en el vault N" para las conexiones de ese vault.
///
/// Es SOLO una señal, nunca una fuente de verdad: el cliente que la recibe hace un pull con su
/// `since` y relee el oplog. Por eso un aviso perdido (receptor lento, canal lleno, nadie
/// escuchando, un cliente que estaba reconectando) no pierde datos — y por eso el plan dice que
/// no se usa un broadcast descartable como fuente de verdad: es un empujón.
#[derive(Clone)]
pub struct Notifier {
    canales: Arc<Mutex<HashMap<i64, broadcast::Sender<Aviso>>>>,
}

/// Un aviso en tránsito: el JSON que viaja al cliente y **quién lo produjo**. El emisor va en el
/// sobre y no dentro del JSON a propósito: la spec del wire no cambia, y así cada conexión puede
/// saltear su propio eco (el pull que dispara un aviso no sirve para quien acaba de subir).
#[derive(Clone)]
pub struct Aviso {
    pub de_device: i64,
    pub texto: Arc<String>,
}

/// Cuántos avisos puede atrasarse un receptor antes de que el canal lo marque `lagged`. Si se
/// atrasa, el cliente igual relee el oplog con su `since`: el aviso es sólo el empujón.
const CAPACIDAD_CANAL: usize = 64;

impl Notifier {
    pub fn new() -> Self {
        Self {
            canales: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Suscribe una conexión a los avisos de un vault.
    pub fn suscribir(&self, vault_id: i64) -> broadcast::Receiver<Aviso> {
        self.canales
            .lock()
            .expect("el lock del notifier no se envenena")
            .entry(vault_id)
            .or_insert_with(|| broadcast::channel(CAPACIDAD_CANAL).0)
            .subscribe()
    }

    /// Avisa que hay revisiones nuevas a todos los suscriptos del vault. El `de_device` que las
    /// produjo viaja en el sobre, así que cada conexión saltea su propio eco.
    pub fn avisar(&self, vault_id: i64, de_device: i64, avisos: &[Arc<String>]) {
        let canales = self
            .canales
            .lock()
            .expect("el lock del notifier no se envenena");
        let Some(canal) = canales.get(&vault_id) else {
            return;
        };
        for texto in avisos {
            // Sin receptores el envío falla y se descarta: es una señal, no una cola.
            let _ = canal.send(Aviso {
                de_device,
                texto: texto.clone(),
            });
        }
    }
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    use tempfile::tempdir;

    fn blob_id(bytes: &[u8]) -> BlobId {
        let digest = Sha256::digest(bytes);
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&digest);
        BlobId::new(hash)
    }

    #[tokio::test]
    async fn publish_y_read_devuelven_lo_mismo() {
        let directory = tempdir().unwrap();
        let store = CasStore::new(directory.path().to_path_buf()).unwrap();
        let bytes = vec![0x5a; 1024];
        let hash = blob_id(&bytes);

        store.publish(&hash, &bytes).await.unwrap();
        let read = store.read(&hash).await.unwrap();

        assert_eq!(read, bytes);
    }

    #[tokio::test]
    async fn publish_es_idempotente_y_no_reescribe() {
        let directory = tempdir().unwrap();
        let store = CasStore::new(directory.path().to_path_buf()).unwrap();
        let bytes = vec![0x31; 1024];
        let hash = blob_id(&bytes);
        let path = store.blob_path(&hash);

        store.publish(&hash, &bytes).await.unwrap();
        let first_mtime = fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap();

        store.publish(&hash, &bytes).await.unwrap();
        let second_mtime = fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap();

        assert_eq!(first_mtime, second_mtime);
        assert_eq!(
            fs::read_dir(path.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn hash_que_no_corresponde_no_deja_residuo() {
        let directory = tempdir().unwrap();
        let store = CasStore::new(directory.path().to_path_buf()).unwrap();
        let bytes = b"contenido cuyo hash no corresponde";
        let hash = BlobId::new([0u8; 32]);

        assert!(store.publish(&hash, bytes).await.is_err());

        let shard = directory.path().join("00");
        assert!(!shard.join(hash.to_string()).exists());

        if shard.exists() {
            assert_eq!(fs::read_dir(shard).unwrap().count(), 0);
        }
    }

    #[tokio::test]
    async fn read_de_hash_inexistente_es_not_found() {
        let directory = tempdir().unwrap();
        let store = CasStore::new(directory.path().to_path_buf()).unwrap();
        let hash = BlobId::new([0xabu8; 32]);

        let error = store.read(&hash).await.unwrap_err();

        assert!(matches!(error, ServerError::NotFound));
    }

    #[tokio::test]
    async fn read_de_blob_corrupto_falla() {
        let directory = tempdir().unwrap();
        let store = CasStore::new(directory.path().to_path_buf()).unwrap();
        let bytes = vec![0x5a; 1024];
        let hash = blob_id(&bytes);
        let path = store.blob_path(&hash);

        store.publish(&hash, &bytes).await.unwrap();

        let mut corrupted = bytes.clone();
        corrupted[0] ^= 0xff;
        fs::write(&path, &corrupted).unwrap();

        let error = store.read(&hash).await.unwrap_err();

        assert!(matches!(
            error,
            ServerError::Invalid(message)
                if message == "el contenido del blob no coincide con su hash"
        ));
    }

    #[tokio::test]
    async fn el_blob_queda_bajo_el_sharding() {
        let directory = tempdir().unwrap();
        let store = CasStore::new(directory.path().to_path_buf()).unwrap();
        let bytes = b"blob bajo sharding";
        let hash = blob_id(bytes);

        store.publish(&hash, bytes).await.unwrap();

        let hex = hash.to_string();
        let expected = directory.path().join(&hex[..2]).join(&hex);
        assert!(expected.is_file());
        assert_eq!(
            fs::metadata(&expected)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(SystemTime::UNIX_EPOCH)
                .is_ok(),
            true
        );
    }
}

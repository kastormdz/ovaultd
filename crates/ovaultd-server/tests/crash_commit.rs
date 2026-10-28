//! Pruebas de crash del server (fase 2, paso 6).
//!
//! Qué se prueba, y contra qué:
//!
//!  - **In-process** (failpoints en modo `panic`): crash después del temporal, después del
//!    fsync, después de publicar en el CAS y después del commit SQL. Los estados durables
//!    quedan EXACTAMENTE en el punto de corte, y los reintentos son idempotentes (el "ACK
//!    perdido" no duplica revisión).
//!  - **Proceso real** (failpoint en modo `abort` = SIGABRT): el binario se cae a mitad de
//!    un PUT; un proceso NUEVO verifica la base (integridad + invariantes) y completa la
//!    subida. El proceso muerto no es una metáfora: es `wait()` con señal.
//!
//! Invariante que se verifica siempre: ninguna revisión confirmada referencia un blob
//! inexistente, y un blob publicado sin commit queda huérfano (recuperable por GC), nunca
//! como archivo visible.
//!
//! Los failpoints se activan por variables de entorno GLOBALES del proceso de test: cada
//! test de este archivo toma `ENV_LOCK` para serializarse y el guard limpia el env al
//! soltarse (también si un assert falla).
//!
//! Nota: `cas.*` vive dentro de `spawn_blocking` — un panic ahí muere en su thread y el
//! handler responde error (los efectos durables ya ocurrieron; el test lee el ESTADO, no
//! el unwind). `sql.commit` corre inline y sí se captura con `catch_unwind`.

mod common;

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Mutex;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::{hex_lower, TestContext};
use futures::FutureExt;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

/// Serializa los tests que tocan `OVAULTD_FAILPOINT*` (env global del proceso).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Serializa los tests que tocan `OVAULTD_FAILPOINT*` (env global del proceso).
///
/// Debe quedarse vivo TODO el test: si se liberara apenas se desactiva el failpoint,
/// otro test setearía SU failpoint mientras este todavía hace retries, y el retry del
/// primero dispararía el failpoint ajeno (el env es del proceso, no del test).
struct Serializado {
    _guard: std::sync::MutexGuard<'static, ()>,
}

fn serializar() -> Serializado {
    Serializado {
        _guard: ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner()),
    }
}

/// Activa un failpoint hasta que se lo dropee; el `Drop` limpia el env.
struct FailpointActivo;

impl FailpointActivo {
    fn new(punto: &str, modo: &str) -> Self {
        // SAFETY (edition 2024): `set_var` es unsafe porque mutar el entorno no es
        // thread-safe en general; acá TODOS los tests que la tocan están serializados
        // por `Serializado` y nadie más del proceso lee estas variables.
        unsafe {
            std::env::set_var("OVAULTD_FAILPOINT", punto);
            std::env::set_var("OVAULTD_FAILPOINT_MODE", modo);
        }
        Self
    }
}

impl Drop for FailpointActivo {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("OVAULTD_FAILPOINT");
            std::env::remove_var("OVAULTD_FAILPOINT_MODE");
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

fn blob_id_de(bytes: &[u8]) -> ovaultd_protocol::BlobId {
    ovaultd_protocol::BlobId::new(Sha256::digest(bytes).into())
}

/// Blob de prueba con contenido determinista (252 bytes, para que no sea trivial).
fn bytes_de_prueba() -> Vec<u8> {
    (0..252u32).map(|i| (i % 251) as u8).collect()
}

/// PUT del blob por la superficie HTTP completa (router real).
async fn put_blob(
    contexto: &TestContext,
    bytes: &[u8],
) -> Result<axum::response::Response, Box<dyn std::error::Error>> {
    let hash = sha256_hex(bytes);
    let uri = format!("/api/v1/vaults/{}/blobs/{hash}", contexto.vault_id);
    contexto
        .authenticated_request(Method::PUT, &uri, Body::from(bytes.to_vec()))
        .await
}

async fn contar(pool: &SqlitePool, tabla: &str) -> i64 {
    // Whitelist de literales: es lo que sqlx 0.9 pide para SQL "dinámico" en tests.
    let sql = match tabla {
        "blobs" => "SELECT count(*) FROM blobs",
        "revisions" => "SELECT count(*) FROM revisions",
        "batches" => "SELECT count(*) FROM batches",
        "vaults" => "SELECT count(*) FROM vaults",
        otra => panic!("tabla no permitida en tests: {otra}"),
    };
    sqlx::query_scalar::<_, i64>(sql)
        .fetch_one(pool)
        .await
        .expect("la tabla existe")
}

/// El invariante de fase 2: ninguna revisión confirmada referencia un blob inexistente
/// (ni en el índice `blobs` ni en el CAS).
async fn referencias_sanas(pool: &SqlitePool, cas: &Path) -> bool {
    let referenciados: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT DISTINCT blob_hash FROM revisions WHERE blob_hash IS NOT NULL
         UNION
         SELECT DISTINCT blob_hash FROM files WHERE blob_hash IS NOT NULL AND deleted = 0",
    )
    .fetch_all(pool)
    .await
    .expect("consulta válida");

    for hash in referenciados {
        let hex = hex_lower(&hash);
        let en_indice: i64 =
            sqlx::query_scalar("SELECT count(*) FROM blobs WHERE hash = ?")
                .bind(&hash)
                .fetch_one(pool)
                .await
                .expect("consulta válida");
        if en_indice != 1 {
            return false;
        }
        if !cas.join(&hex[..2]).join(&hex).is_file() {
            return false;
        }
    }
    true
}

// --------------------------------------------------------------------------- in-process

/// Crash después de publicar en el CAS pero ANTES de registrar el blob: queda huérfano
/// (en el CAS, sin fila), ninguna revisión lo referencia, y re-subir el mismo blob cura
/// el índice.
#[tokio::test]
async fn crash_tras_publicar_en_cas_deja_huerfano_y_el_retry_cura() {
    let _serial = serializar();
    let contexto = TestContext::new().await.expect("contexto");
    let bytes = bytes_de_prueba();
    let blob = blob_id_de(&bytes);
    let activo = FailpointActivo::new("cas.publish", "panic");

    let respuesta = put_blob(&contexto, &bytes).await.expect("request");
    assert!(
        !respuesta.status().is_success(),
        "el crash del CAS no puede responder 2xx ({})",
        respuesta.status()
    );

    // Estado durable EXACTO del punto de corte: bytes en el CAS, sin fila en el índice.
    assert!(contexto.cas.exists(&blob), "los bytes ya estaban publicados");
    assert_eq!(contar(&contexto.pool, "blobs").await, 0, "sin fila en blobs");
    assert_eq!(contar(&contexto.pool, "revisions").await, 0);
    assert!(referencias_sanas(&contexto.pool, contexto.cas.raiz()).await);

    drop(activo);
    // El blob ya está en el CAS: re-subirlo es idempotente y ADEMÁS cura el índice.
    let retry = put_blob(&contexto, &bytes).await.expect("retry");
    assert_eq!(retry.status(), StatusCode::OK);
    assert_eq!(contar(&contexto.pool, "blobs").await, 1);
    assert!(referencias_sanas(&contexto.pool, contexto.cas.raiz()).await);
}

/// Crash antes del fsync y antes del rename: NADA quedó publicado (solo el temporal
/// huérfano), y el reintento publica normalmente.
/// El mismo caso, un punto más temprano: crash después de escribir el temporal.
#[tokio::test]
async fn crash_tras_el_temporal_no_deja_blob() {
    let _serial = serializar();
    let contexto = TestContext::new().await.expect("contexto");
    let bytes = bytes_de_prueba();
    let blob = blob_id_de(&bytes);
    let activo = FailpointActivo::new("cas.temp", "panic");

    let respuesta = put_blob(&contexto, &bytes).await.expect("request");
    assert!(!respuesta.status().is_success());

    assert!(!contexto.cas.exists(&blob));
    assert_eq!(contar(&contexto.pool, "blobs").await, 0);

    drop(activo);
    let retry = put_blob(&contexto, &bytes).await.expect("retry");
    assert_eq!(retry.status(), StatusCode::CREATED);
    assert_eq!(contar(&contexto.pool, "blobs").await, 1);
}

/// Crash DESPUÉS del commit SQL y antes del ack: la revisión quedó confirmada, el mismo
/// batch reintentado (mismo `operation_id`) devuelve el MISMO rev y no duplica nada.
#[tokio::test]
async fn crash_tras_el_commit_sql_no_duplica_al_reintentar() {
    let _serial = serializar();
    let contexto = TestContext::new().await.expect("contexto");

    // Blob REAL: publicado en el CAS y registrado (como una subida de verdad), para que
    // el invariante "revisión confirmada ⇒ blob existente" sea verificable de punta a punta.
    let contenido = bytes_de_prueba();
    let blob = blob_id_de(&contenido);
    contexto.cas.publish(&blob, &contenido).await.expect("blob publicado");
    ovaultd_server::storage::registrar_blob(
        &contexto.pool,
        contexto.vault_id,
        &blob,
        contenido.len() as i64,
    )
    .await
    .expect("blob registrado");

    // Mismo batch firmado para el commit y para el reintento (el operation_id es el mismo).
    let batch = contexto.batch_firmado(
        vec![common::add_op_con_blob(7, blob, contenido.len() as u64)],
        0,
    );
    let _activo = FailpointActivo::new("sql.commit", "panic");

    let resultado = AssertUnwindSafe(ovaultd_server::sync::commit::commit_batch(
        &contexto.pool,
        contexto.vault_id,
        contexto.device_id,
        &batch,
    ))
    .catch_unwind()
    .await;
    assert!(resultado.is_err(), "el failpoint tiene que panickear");

    drop(_activo);

    // Estado durable del punto de corte: TODO commiteado (una sola revisión).
    assert_eq!(contar(&contexto.pool, "revisions").await, 1);
    assert_eq!(contar(&contexto.pool, "batches").await, 1);
    let head: i64 = sqlx::query_scalar("SELECT head_rev FROM vaults WHERE id = ?")
        .bind(contexto.vault_id)
        .fetch_one(&contexto.pool)
        .await
        .expect("vault");
    assert_eq!(head, 1);
    assert!(referencias_sanas(&contexto.pool, contexto.cas.raiz()).await);

    // "ACK perdido": reintento del MISMO batch ⇒ idempotente, mismo rev, sin duplicar.
    let reintento = ovaultd_server::sync::commit::commit_batch(
        &contexto.pool,
        contexto.vault_id,
        contexto.device_id,
        &batch,
    )
    .await
    .expect("reintento");
    assert!(reintento.idempotente);
    assert_eq!(reintento.rev, 1);
    assert_eq!(contar(&contexto.pool, "revisions").await, 1);
    assert_eq!(contar(&contexto.pool, "batches").await, 1);
}

// --------------------------------------------------------------------------- proceso real

fn puerto_libre() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("puerto efímero");
    listener.local_addr().expect("addr").port()
}

fn curl(args: &[&str]) -> std::process::Output {
    std::process::Command::new("curl")
        .args(args)
        .output()
        .expect("curl disponible (lo usan todos los scripts del repo)")
}

fn esperar_healthz(base: &str) {
    for _ in 0..100 {
        let salida = curl(&["-sf", "-o", "/dev/null", &format!("{base}/healthz")]);
        if salida.status.success() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("el server no respondió /healthz en 10 s ({base})");
}

/// Espera la muerte del server con timeout: un `wait()` pelado cuelga para siempre si el
/// failpoint no llegó a dispararse (y esconde el error real detrás de un timeout de CI).
fn esperar_muerte(hijo: &mut std::process::Child) -> std::process::ExitStatus {
    for _ in 0..100 {
        if let Some(estado) = hijo.try_wait().expect("try_wait") {
            return estado;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = hijo.kill();
    let _ = hijo.wait();
    panic!("el server no murió en 10 s: ¿el failpoint no se disparó?");
}

/// El binario REAL se cae a mitad de un PUT (SIGABRT, sin cleanup); un proceso nuevo
/// verifica la base intacta y completa la subida pendiente.
#[tokio::test]
async fn proceso_real_abortado_a_mitad_de_un_put_deja_la_base_usable() {
    use std::os::unix::process::ExitStatusExt;

    let _serial = serializar();

    let dir = tempfile::tempdir().expect("tmpdir");
    let db_path = dir.path().join("metadata.db");
    let cas_dir = dir.path().join("cas");
    std::fs::create_dir_all(&cas_dir).expect("cas dir");
    let puerto = puerto_libre();
    let base = format!("http://127.0.0.1:{puerto}");

    // Cuenta + vault + token, escritos por fuera del server (lo que haría el admin CLI).
    let (token, _device) = {
        let pool = ovaultd_server::db::connect(db_path.to_str().expect("ruta"))
            .await
            .expect("db");
        ovaultd_server::db::migrate(&pool).await.expect("migraciones");
        let cuenta = ovaultd_server::auth::crear_cuenta(
            &pool,
            common::EMAIL,
            common::PASSWORD,
        )
        .await
        .expect("cuenta");
        let (token, device_id) = ovaultd_server::auth::iniciar_sesion(
            &pool,
            common::EMAIL,
            common::PASSWORD,
            "test-crash",
            Some("integracion"),
        )
        .await
        .expect("token");
        sqlx::query(
            "INSERT INTO vaults
                 (account_id, name, salt, kdf_params, wrapped_key_pw, wrapped_key_rc, vault_pubkey)
             VALUES (?, 'vault-crash', ?, '{}', ?, ?, ?)",
        )
        .bind(cuenta.id)
        .bind(vec![1u8; 16])
        .bind(vec![0u8; 32])
        .bind(vec![0u8; 32])
        .bind(vec![9u8; 32])
        .execute(&pool)
        .await
        .expect("vault");
        pool.close().await;
        (token, device_id)
    };

    let bytes = bytes_de_prueba();
    let hash = sha256_hex(&bytes);
    let cuerpo = dir.path().join("cuerpo.bin");
    std::fs::write(&cuerpo, &bytes).expect("cuerpo");

    let mut servidor = std::process::Command::new(env!("CARGO_BIN_EXE_ovaultd"))
        .env("OVAULTD_BIND", format!("127.0.0.1:{puerto}"))
        .env("OVAULTD_DB", &db_path)
        .env("OVAULTD_CAS", &cas_dir)
        .env_remove("OVAULTD_FAILPOINT_MODE")
        .env("OVAULTD_FAILPOINT", "cas.publish")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("el binario del server existe");
    esperar_healthz(&base);

    // El PUT muere a mitad: el proceso aborta DESPUÉS de publicar (failpoint cas.publish).
    let cuerpo_arg = format!("@{}", cuerpo.display());
    let respuesta = curl(&[
        "-s",
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}",
        "-X",
        "PUT",
        &format!("{base}/api/v1/vaults/1/blobs/{hash}"),
        "-H",
        &format!("authorization: Bearer {token}"),
        "-H",
        "content-type: application/octet-stream",
        "--data-binary",
        &cuerpo_arg,
    ]);
    let codigo = String::from_utf8_lossy(&respuesta.stdout);
    assert_ne!(codigo.trim(), "201", "el PUT no puede completar con el failpoint activo");

    let estado = esperar_muerte(&mut servidor);
    assert!(!estado.success());
    assert_eq!(estado.signal(), Some(6), "abort() ⇒ SIGABRT (status {estado:?})");

    // Con el proceso MUERTO: la base está íntegra y el blob quedó huérfano (sin fila).
    let pool = ovaultd_server::db::connect(db_path.to_str().expect("ruta"))
        .await
        .expect("reconexión");
    let integridad: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await
        .expect("integrity_check corre");
    assert_eq!(integridad, "ok");
    assert_eq!(contar(&pool, "blobs").await, 0);
    assert_eq!(contar(&pool, "revisions").await, 0);
    assert_eq!(contar(&pool, "batches").await, 0);
    assert_eq!(contar(&pool, "vaults").await, 1);
    assert!(referencias_sanas(&pool, &cas_dir).await);
    pool.close().await;

    let publicado = cas_dir.join(&hash[..2]).join(&hash);
    assert!(publicado.is_file(), "el blob quedó publicado en el CAS");
    assert_eq!(std::fs::read(&publicado).expect("bytes"), bytes);

    // Proceso NUEVO, sin failpoint: la subida pendiente se completa y el índice se cura.
    let mut servidor2 = std::process::Command::new(env!("CARGO_BIN_EXE_ovaultd"))
        .env("OVAULTD_BIND", format!("127.0.0.1:{puerto}"))
        .env("OVAULTD_DB", &db_path)
        .env("OVAULTD_CAS", &cas_dir)
        .env_remove("OVAULTD_FAILPOINT")
        .env_remove("OVAULTD_FAILPOINT_MODE")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("respawn");
    esperar_healthz(&base);

    let retry = curl(&[
        "-s",
        "-o",
        "/dev/null",
        "-w",
        "%{http_code}",
        "-X",
        "PUT",
        &format!("{base}/api/v1/vaults/1/blobs/{hash}"),
        "-H",
        &format!("authorization: Bearer {token}"),
        "-H",
        "content-type: application/octet-stream",
        "--data-binary",
        &cuerpo_arg,
    ]);
    assert_eq!(String::from_utf8_lossy(&retry.stdout).trim(), "200", "idempotente + cura");

    // Y el blob ahora es legible por la superficie real.
    let descarga = curl(&[
        "-sf",
        &format!("{base}/api/v1/vaults/1/blobs/{hash}"),
        "-H",
        &format!("authorization: Bearer {token}"),
    ]);
    assert!(descarga.status.success());
    assert_eq!(descarga.stdout, bytes);

    let pool = ovaultd_server::db::connect(db_path.to_str().expect("ruta"))
        .await
        .expect("reconexión");
    assert_eq!(contar(&pool, "blobs").await, 1);
    pool.close().await;

    let _ = servidor2.kill();
    let _ = servidor2.wait();
}

/// El `operation_id` es la clave de idempotencia: un reintento con LAS MISMAS ops devuelve la
/// misma revisión sin escribir nada. Pero si el cliente reusa el id con otras ops —un estado
/// local restaurado de un backup viejo, un bug de generación— la versión anterior devolvía
/// `idempotente: true` y **descartaba los cambios nuevos en silencio**.
#[tokio::test]
async fn un_operation_id_reusado_con_otras_ops_no_descarta_los_cambios(
) -> Result<(), Box<dyn std::error::Error>> {
    let contexto = TestContext::new().await?;
    let contenido = b"uno";
    contexto
        .registrar_blob(9, contenido.len() as i64)
        .await
        .expect("blob");

    let primero = contexto
        .batch_firmado(vec![common::add_op(7, 9, contenido.len() as u64)], 0);
    let r1 = ovaultd_server::sync::commit::commit_batch(
        &contexto.pool,
        contexto.vault_id,
        contexto.device_id,
        &primero,
    )
    .await
    .expect("primer commit");
    assert_eq!(r1.rev, 1);

    // Mismo operation_id, OTRA operación (path 8 en vez de 7), bien firmada.
    let mut segundo = contexto
        .batch_firmado(vec![common::add_op(8, 9, contenido.len() as u64)], 1);
    segundo.operation_id = primero.operation_id.clone();
    let op_id = uuid::Uuid::parse_str(&segundo.operation_id).expect("uuid");
    for op in &mut segundo.ops {
        common::firmar_op(op, &contexto.identidad(), 1, op_id.as_bytes(), contexto.epoch);
    }

    let resultado = ovaultd_server::sync::commit::commit_batch(
        &contexto.pool,
        contexto.vault_id,
        contexto.device_id,
        &segundo,
    )
    .await;

    assert!(
        resultado.is_err(),
        "un operation_id con contenido distinto no puede pasar como idempotente: {:?}",
        resultado.map(|r| r.rev)
    );
    // Y el estado no cambió: sigue una sola revisión, la del primer batch.
    assert_eq!(contar(&contexto.pool, "revisions").await, 1);
    let paths: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE vault_id = ?")
        .bind(contexto.vault_id)
        .fetch_one(&contexto.pool)
        .await
        .expect("files");
    assert_eq!(paths, 1, "el path 8 no se aplicó");

    Ok(())
}

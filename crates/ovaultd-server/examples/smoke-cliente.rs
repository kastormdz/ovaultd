//! Cliente de prueba para `tests/scripts/server-smoke.sh`.
//!
//! No es un cliente de producto: es el mínimo que firma lo que el servidor exige verificar,
//! para que el smoke ejerza el protocolo completo contra el BINARIO (curl para HTTP, este
//! cliente para la cripto y para la sesión WS).
//!
//! Subcomandos (el estado del vault persiste en `--state`):
//! - `init`: crea la cuenta (bootstrap de admin: no hay registro público) y el material del vault.
//! - `material`: imprime el cuerpo de `POST /vaults`.
//! - `proof`: firma el challenge para `POST /access`.
//! - `encrypt`: cifra un archivo de contenido y escribe el blob (ciphertext).
//! - `commit`: sesión WS completa (hello -> auth -> batch) y el ack en JSON.
//!
//! Nada de firmas constantes: cada corrida genera claves y nonces nuevos, y el contenido
//! se cifra como lo haría el cliente real (`sync2`).

use std::{
    env, fs,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::http::{header::AUTHORIZATION, HeaderValue};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use ovaultd_crypto::{
    derive_kek, derive_kek_recovery, derive_subkeys, encrypt_blob, sign_proof, sign_record,
    VaultIdentity, VaultKey,
};
use ovaultd_protocol::{Batch, BatchOp, BlobId, Op, PathId};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, protocol::Message},
    WebSocketStream,
};
use uuid::Uuid;

type Ws = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// `vaults.epoch` arranca en 1 (`001_init.sql`); la rotación (fase 4) lo va a mover.
const EPOCH_INICIAL: u16 = 1;

/// El código de operación del WIRE es el mapeo de `commit.rs::verify_batch_operation`
/// (`Op::Add => 0`), **no** el discriminante del enum (`Op::Add as u8 == 1`).
const OP_CODE_ADD: u8 = 0;

#[derive(Debug, Serialize, Deserialize)]
struct KdfJson {
    m_kib: u32,
    t: u32,
    p: u32,
    dk_len: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct Estado {
    email: String,
    password: String,
    vault_key: String,
    vault_uid: String,
    salt: String,
    kdf: KdfJson,
    wrapped_key_pw: String,
    wrapped_key_rc: String,
    pubkey: String,
    recovery_secret: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = correr().await {
        eprintln!("smoke-cliente: {error}");
        std::process::exit(1);
    }
}

async fn correr() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((subcomando, resto)) = args.split_first() else {
        return Err("falta el subcomando (init|material|proof|encrypt|commit)".to_owned());
    };

    match subcomando.as_str() {
        "init" => init(resto).await,
        "material" => material(resto),
        "proof" => proof(resto),
        "encrypt" => encrypt(resto),
        "commit" => commit(resto).await,
        otro => Err(format!("subcomando desconocido: {otro}")),
    }
}

// ---------------------------------------------------------------- infra minima

fn flag(args: &[String], nombre: &str) -> Result<String, String> {
    let posicion = args
        .iter()
        .position(|arg| arg == nombre)
        .ok_or_else(|| format!("falta {nombre}"))?;

    args.get(posicion + 1)
        .cloned()
        .ok_or_else(|| format!("{nombre} sin valor"))
}

fn flag_i64(args: &[String], nombre: &str) -> Result<i64, String> {
    flag(args, nombre)?
        .parse::<i64>()
        .map_err(|error| format!("{nombre}: {error}"))
}

fn flag_u64(args: &[String], nombre: &str) -> Result<u64, String> {
    flag(args, nombre)?
        .parse::<u64>()
        .map_err(|error| format!("{nombre}: {error}"))
}

fn e2s(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn b64e(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

fn b64d(texto: &str) -> Result<Vec<u8>, String> {
    STANDARD
        .decode(texto)
        .map_err(|error| format!("base64 inválido: {error}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn bytes16(texto: &str) -> Result<[u8; 16], String> {
    b64d(texto)?
        .try_into()
        .map_err(|_| "se esperaban 16 bytes".to_owned())
}

fn bytes32(texto: &str) -> Result<[u8; 32], String> {
    b64d(texto)?
        .try_into()
        .map_err(|_| "se esperaban 32 bytes".to_owned())
}

fn leer_estado(ruta: &str) -> Result<Estado, String> {
    let texto = fs::read_to_string(ruta).map_err(|error| format!("{ruta}: {error}"))?;
    serde_json::from_str(&texto).map_err(|error| format!("{ruta} no es JSON válido: {error}"))
}

fn claves(estado: &Estado) -> Result<ovaultd_crypto::SubKeys, String> {
    let vault_key = VaultKey(bytes32(&estado.vault_key)?);
    Ok(derive_subkeys(&vault_key, &bytes16(&estado.salt)?))
}

fn identidad(estado: &Estado) -> Result<VaultIdentity, String> {
    Ok(VaultIdentity::generate(&claves(estado)?.sign))
}

fn ahora_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duracion| u64::try_from(duracion.as_millis()).ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------- subcomandos

/// Crea la cuenta y genera el material del vault (claves reales, no constantes).
async fn init(args: &[String]) -> Result<(), String> {
    let ruta_estado = flag(args, "--state")?;
    let ruta_db = flag(args, "--db")?;
    let email = flag(args, "--email")?;
    let password = flag(args, "--password")?;

    let pool = ovaultd_server::db::connect(&ruta_db)
        .await
        .map_err(e2s)?;
    ovaultd_server::db::migrate(&pool).await.map_err(e2s)?;
    ovaultd_server::auth::crear_cuenta(&pool, &email, &password)
        .await
        .map_err(e2s)?;

    // Aleatoriedad de prueba vía uuid / VaultKey::generate: ovaultd-crypto no expone RNG propio.
    let vault_key = VaultKey::generate().map_err(e2s)?;
    let salt = *Uuid::new_v4().as_bytes();
    let vault_uid = *Uuid::new_v4().as_bytes();
    let recovery_secret = VaultKey::generate().map_err(e2s)?.0;

    // Parámetros chicos A PROPÓSITO: el smoke prueba el FLUJO, no el costo del KDF.
    // (Los defaults de sync2 — 64 MiB / t=3 / p=4 — en un build debug tardan de más;
    // el interop de los defaults ya lo cubre el gate cripto con sus vectores.)
    let kdf = ovaultd_crypto::KdfParams {
        m_kib: 8 * 1024,
        t: 1,
        p: 1,
        dk_len: 32,
    };

    let kek = derive_kek(&password, &salt, &kdf).map_err(e2s)?;
    let wrapped_key_pw = vault_key.wrap_pw(&kek, &vault_uid, &salt).map_err(e2s)?;
    let kek_recovery = derive_kek_recovery(&recovery_secret, &salt);
    let wrapped_key_rc = vault_key
        .wrap_rc(&kek_recovery, &vault_uid)
        .map_err(e2s)?;

    let identidad = VaultIdentity::generate(&derive_subkeys(&vault_key, &salt).sign);

    let estado = Estado {
        email,
        password,
        vault_key: b64e(&vault_key.0),
        vault_uid: b64e(&vault_uid),
        salt: b64e(&salt),
        kdf: KdfJson {
            m_kib: kdf.m_kib,
            t: kdf.t,
            p: kdf.p,
            dk_len: kdf.dk_len,
        },
        wrapped_key_pw: b64e(&wrapped_key_pw),
        wrapped_key_rc: b64e(&wrapped_key_rc),
        pubkey: b64e(&identidad.pubkey()),
        recovery_secret: b64e(&recovery_secret),
    };

    let texto = serde_json::to_string_pretty(&estado).map_err(e2s)?;
    fs::write(&ruta_estado, texto).map_err(|error| format!("{ruta_estado}: {error}"))?;

    eprintln!("  cuenta {} creada en {ruta_db}", estado.email);
    eprintln!("  material del vault -> {ruta_estado}");
    Ok(())
}

/// Imprime el cuerpo de `POST /vaults` (lo manda `curl` en el smoke).
fn material(args: &[String]) -> Result<(), String> {
    let estado = leer_estado(&flag(args, "--state")?)?;

    let cuerpo = json!({
        "name": "smoke",
        "salt": estado.salt,
        "kdf_params": {
            "m_kib": estado.kdf.m_kib,
            "t": estado.kdf.t,
            "p": estado.kdf.p,
            "dk_len": estado.kdf.dk_len
        },
        "wrapped_key_pw": estado.wrapped_key_pw,
        "wrapped_key_rc": estado.wrapped_key_rc,
        "vault_pubkey": estado.pubkey
    });

    println!("{}", serde_json::to_string(&cuerpo).map_err(e2s)?);
    Ok(())
}

/// Firma el challenge pendiente para `POST /access`.
fn proof(args: &[String]) -> Result<(), String> {
    let estado = leer_estado(&flag(args, "--state")?)?;
    let vault = flag_i64(args, "--vault")?;
    let device = flag_i64(args, "--device")?;
    let challenge = b64d(&flag(args, "--challenge")?)?;

    let firma = sign_proof(&claves(&estado)?, vault, device, &challenge);

    println!("{}", json!({ "proof": b64e(&firma) }));
    Ok(())
}

/// Cifra un archivo de contenido y escribe el blob; imprime el `blob_id` en hex
/// (es el que va en el path del `PUT`).
fn encrypt(args: &[String]) -> Result<(), String> {
    let estado = leer_estado(&flag(args, "--state")?)?;
    let origen = flag(args, "--content-file")?;
    let destino = flag(args, "--out")?;

    let contenido = fs::read(&origen).map_err(|error| format!("{origen}: {error}"))?;
    let blob = encrypt_blob(&claves(&estado)?, &contenido).map_err(e2s)?;
    fs::write(&destino, &blob).map_err(|error| format!("{destino}: {error}"))?;

    eprintln!(
        "  cifrado {} B -> {destino}",
        blob.len()
    );
    println!("{}", hex(&ovaultd_crypto::blob_id(&blob)));
    Ok(())
}

/// Sesión WS completa contra el servidor y el ack en JSON.
///
/// Lo que firma es lo que manda: el `BatchOp` que se serializa es el mismo que se firmó
/// (`sign_record` sobre `msg_rec`), y el `t_mac` es el binding real `path_id <-> blob_id`.
async fn commit(args: &[String]) -> Result<(), String> {
    let estado = leer_estado(&flag(args, "--state")?)?;
    let ws_url = flag(args, "--ws-url")?;
    let vault = flag_i64(args, "--vault")?;
    let device = flag_i64(args, "--device")?;
    let token = flag(args, "--token")?;
    let path = flag(args, "--path")?;
    let blob_file = flag(args, "--blob-file")?;
    let expected_rev = flag_u64(args, "--expected-rev")?;

    let keys = claves(&estado)?;
    let identidad = identidad(&estado)?;
    let cipher = fs::read(&blob_file).map_err(|error| format!("{blob_file}: {error}"))?;

    let blob_id = ovaultd_crypto::blob_id(&cipher);
    let path_id = ovaultd_crypto::path_id(&keys, &path);
    let path_enc = ovaultd_crypto::path_enc(&keys, &path).map_err(e2s)?;
    let t_mac = ovaultd_crypto::t_mac(
        &identidad,
        &bytes16(&estado.vault_uid)?,
        &path_id,
        &blob_id,
        EPOCH_INICIAL,
    );

    let operation_id = Uuid::new_v4();
    let mtime = ahora_ms();
    let size = u64::try_from(cipher.len()).map_err(e2s)?;
    let sig = sign_record(
        &identidad,
        &path_id,
        &blob_id,
        &t_mac,
        OP_CODE_ADD,
        size,
        mtime,
        expected_rev,
        operation_id.as_bytes(),
        EPOCH_INICIAL,
    );

    let op = BatchOp {
        op: Op::Add,
        path_id: PathId::new(path_id),
        path_enc: Some(b64e(&path_enc)),
        folder: false,
        blob_id: Some(BlobId::new(blob_id)),
        t_mac: Some(b64e(&t_mac)),
        sig: b64e(&sig),
        size: Some(size),
        mtime: Some(mtime),
        ctime: Some(mtime),
        prev_path_id: None,
    };

    let batch = Batch {
        t: "batch".to_owned(),
        batch_id: Uuid::new_v4().to_string(),
        operation_id: operation_id.to_string(),
        expected_rev,
        ops: vec![op],
    };
    batch
        .validate()
        .map_err(|error| format!("el batch del smoke no valida: {error:?}"))?;

    // ---------------- WS: hello -> auth -> batch

    let mut request = ws_url
        .clone()
        .into_client_request()
        .map_err(|error| format!("WS URL inválida: {error}"))?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|error| format!("token inválido: {error}"))?,
    );

    let (mut socket, _) = connect_async(request)
        .await
        .map_err(|error| format!("no se pudo conectar el WS: {error}"))?;

    enviar(
        &mut socket,
        &json!({
            "t": "hello",
            "proto": 1,
            "vault": vault,
            "device": device,
            "since": expected_rev
        }),
    )
    .await?;

    let hello_ok = esperar_tipo(&mut socket, "hello_ok").await?;
    let head_rev = hello_ok["head_rev"]
        .as_u64()
        .ok_or("hello_ok sin head_rev")?;
    if head_rev != expected_rev {
        return Err(format!(
            "el head del servidor es {head_rev}; el cliente esperaba la base {expected_rev}"
        ));
    }

    let challenge = b64d(hello_ok["challenge"].as_str().ok_or("hello_ok sin challenge")?)?;
    let proof = sign_proof(&keys, vault, device, &challenge);

    enviar(&mut socket, &json!({ "t": "auth", "proof": b64e(&proof) })).await?;
    esperar_tipo(&mut socket, "auth_ok").await?;

    enviar(&mut socket, &serde_json::to_value(&batch).map_err(e2s)?).await?;
    let ack = esperar_tipo(&mut socket, "batch_ack").await?;

    let _ = socket.close(None).await;

    println!(
        "{}",
        json!({
            "t": "batch_ack",
            "batch_id": ack["batch_id"],
            "rev": ack["rev"],
            "path_id": b64e(&path_id),
            "blob_id": b64e(&blob_id),
            "op": "add",
            "size": size,
            "mtime": mtime,
            "expected_rev": expected_rev
        })
    );
    Ok(())
}

// ---------------------------------------------------------------- WS helpers

async fn enviar(socket: &mut Ws, valor: &Value) -> Result<(), String> {
    socket
        .send(Message::Text(valor.to_string().into()))
        .await
        .map_err(|error| format!("WS send: {error}"))
}

/// Espera el próximo mensaje del tipo pedido. `error` del servidor corta con el detalle;
/// los `notify`/`ack` en vuelo se saltean (no son la respuesta que se espera).
async fn esperar_tipo(socket: &mut Ws, tipo: &str) -> Result<Value, String> {
    while let Some(mensaje) = socket.next().await {
        let mensaje = mensaje.map_err(|error| format!("WS recv: {error}"))?;

        if let Message::Text(texto) = mensaje {
            let valor: Value = serde_json::from_str(texto.as_str())
                .map_err(|error| format!("WS JSON: {error}"))?;

            match valor["t"].as_str() {
                Some(recibido) if recibido == tipo => return Ok(valor),
                Some("error") => return Err(format!("el servidor rechazó: {valor}")),
                Some("notify") | Some("ack") => continue,
                Some(otro) => return Err(format!("mensaje inesperado '{otro}': {valor}")),
                None => return Err(format!("mensaje sin tipo: {valor}")),
            }
        }
    }

    Err(format!("el socket se cerró esperando {tipo}"))
}

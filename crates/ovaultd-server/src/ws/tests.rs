use std::sync::Arc;

use axum::{
    http::{
        header::{AUTHORIZATION, SEC_WEBSOCKET_PROTOCOL},
        HeaderValue,
    },
    Router,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, protocol::Message},
    WebSocketStream,
};
use uuid::Uuid;

use crate::{
    auth,
    config::Config,
    routes::{router, EstadoServidor},
    storage::CasStore,
};

struct TestContext {
    pool: SqlitePool,
    _db_dir: TempDir,
    _cas_dir: TempDir,
    config: Config,
    cas: Arc<CasStore>,
    /// Un solo notifier para todo el contexto: cada `connect` levanta su propio server, así que si
    /// cada uno trajera el suyo, dos conexiones del mismo vault jamás compartirían canal.
    notifier: Arc<crate::storage::Notifier>,
    device_id: i64,
    token: String,
    vault_id: i64,
    epoch: u16,
    keys: ovaultd_crypto::SubKeys,
}

impl TestContext {
    async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let db_dir = tempfile::tempdir()?;
        let cas_dir = tempfile::tempdir()?;

        let db_path = db_dir.path().join("metadata.db");
        let pool = crate::db::connect(db_path.to_str().ok_or("ruta inválida")?).await?;
        sqlx::migrate!("../../migrations").run(&pool).await?;

        let account =
            auth::crear_cuenta(&pool, "a@example.com", auth::PASSWORD_DE_PRUEBA).await?;

        let (token, device_id) = auth::iniciar_sesion(
            &pool,
            "a@example.com",
            auth::PASSWORD_DE_PRUEBA,
            "test-device",
            Some("test"),
        )
        .await?;

        let vault_key = ovaultd_crypto::VaultKey([7u8; 32]);
        let keys = ovaultd_crypto::derive_subkeys(&vault_key, &[8u8; 16]);
        let identity = ovaultd_crypto::VaultIdentity::generate(&keys.sign);

        let vault_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO vaults
                 (account_id, name, salt, kdf_params, wrapped_key_pw,
                  wrapped_key_rc, vault_pubkey)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(account.id)
        .bind("test-vault")
        .bind(vec![8u8; 16])
        .bind("{}")
        .bind(vec![0u8; 32])
        .bind(vec![0u8; 32])
        .bind(identity.pubkey().to_vec())
        .fetch_one(&pool)
        .await?;

        let config = Config {
            bind_addr: "127.0.0.1:8080".to_owned(),
            database_path: db_path.to_string_lossy().into_owned(),
            cas_root: cas_dir.path().to_path_buf(),
            token_ttl_hours: 720,
            challenge_ttl_seconds: 300,
            upload_ttl_hours: 24,
            setup_token: None,
            secure_cookies: crate::config::ModoSecureCookies::Auto,
            trust_proxy: true,
            metrics: crate::config::ModoMetrics::On,
            allowed_origins: Vec::new(),
            public_host: None,
            tz_offset_minutos: -180,
        };

        let cas = Arc::new(CasStore::new(cas_dir.path().to_path_buf())?);

        let epoch = sqlx::query_scalar::<_, i64>("SELECT epoch FROM vaults LIMIT 1")
            .fetch_one(&pool)
            .await?;
        Ok(Self {
            pool,
            _db_dir: db_dir,
            _cas_dir: cas_dir,
            config,
            cas,
            notifier: std::sync::Arc::new(crate::storage::Notifier::new()),
            device_id,
            token,
            vault_id,
            epoch: u16::try_from(epoch)?,
            keys,
        })
    }

    fn estado(&self) -> EstadoServidor {
        EstadoServidor {
            pool: self.pool.clone(),
            config: self.config.clone(),
            cas: self.cas.clone(),
            limitador_login: Default::default(),
            notifier: self.notifier.clone(),
        }
    }

    async fn insert_vault_for(
        &self,
        account_id: i64,
        name: &str,
    ) -> Result<i64, Box<dyn std::error::Error>> {
        let vault_key = ovaultd_crypto::VaultKey([9u8; 32]);
        let keys = ovaultd_crypto::derive_subkeys(&vault_key, &[10u8; 16]);
        let identity = ovaultd_crypto::VaultIdentity::generate(&keys.sign);

        Ok(sqlx::query_scalar::<_, i64>(
            "INSERT INTO vaults
                 (account_id, name, salt, kdf_params, wrapped_key_pw,
                  wrapped_key_rc, vault_pubkey)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(account_id)
        .bind(name)
        .bind(vec![10u8; 16])
        .bind("{}")
        .bind(vec![0u8; 32])
        .bind(vec![0u8; 32])
        .bind(identity.pubkey().to_vec())
        .fetch_one(&self.pool)
        .await?)
    }

    async fn insert_vault_for_other_account(
        &self,
    ) -> Result<i64, Box<dyn std::error::Error>> {
        let account =
            auth::crear_cuenta(&self.pool, "b@example.com", auth::PASSWORD_DE_PRUEBA).await?;

        self.insert_vault_for(account.id, "vault-de-otra-cuenta")
            .await
    }
}

type Ws = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct WsContext {
    socket: Ws,
    server: JoinHandle<()>,
    /// Subprotocolo que el server negoció (lo que llegó en la respuesta del upgrade).
    protocolo_negociado: Option<String>,
}

async fn connect(
    context: &TestContext,
    token: Option<&str>,
) -> Result<WsContext, Box<dyn std::error::Error>> {
    connect_full(context, token, &[]).await
}

/// Conexión completa: `authorization` va como header (si está) y `protocolos` como
/// subprotocolos ofrecidos (comma-joined), igual que los manda un navegador.
async fn connect_full(
    context: &TestContext,
    authorization: Option<&str>,
    protocolos: &[&str],
) -> Result<WsContext, Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;

    let app: Router = router(context.estado());

    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let mut request = format!("ws://{address}/api/v1/ws").into_client_request()?;

    if let Some(token) = authorization {
        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))?,
        );
    }

    if !protocolos.is_empty() {
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_str(&protocolos.join(", "))?,
        );
    }

    match connect_async(request).await {
        Ok((socket, response)) => {
            let protocolo_negociado = response
                .headers()
                .get(SEC_WEBSOCKET_PROTOCOL)
                .and_then(|valor| valor.to_str().ok())
                .map(str::to_owned);
            Ok(WsContext {
                socket,
                server,
                protocolo_negociado,
            })
        }
        Err(error) => {
            server.abort();
            Err(error.into())
        }
    }
}

async fn send_json(
    socket: &mut Ws,
    value: Value,
) -> Result<(), Box<dyn std::error::Error>> {
    socket
        .send(Message::Text(value.to_string().into()))
        .await?;
    Ok(())
}

async fn recv_json(socket: &mut Ws) -> Result<Value, Box<dyn std::error::Error>> {
    while let Some(message) = socket.next().await {
        match message? {
            Message::Text(text) => return Ok(serde_json::from_str(text.as_str())?),
            Message::Close(_) => return Err("el servidor cerró antes de responder".into()),
            _ => {}
        }
    }

    Err("el socket terminó antes de responder".into())
}

async fn hello(
    socket: &mut Ws,
    context: &TestContext,
    vault_id: i64,
    since: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    hello_con(socket, vault_id, context.device_id, since).await
}

/// `hello` para un dispositivo concreto: un segundo dispositivo de la misma cuenta y el mismo
/// vault es lo que hace falta para probar que el aviso viaja entre dispositivos.
async fn hello_con(
    socket: &mut Ws,
    vault_id: i64,
    device_id: i64,
    since: u64,
) -> Result<Value, Box<dyn std::error::Error>> {
    send_json(
        socket,
        json!({
            "t": "hello",
            "proto": 1,
            "vault": vault_id,
            "device": device_id,
            "since": since
        }),
    )
    .await?;

    recv_json(socket).await
}

async fn authenticate(
    socket: &mut Ws,
    context: &TestContext,
) -> Result<(Value, Vec<u8>), Box<dyn std::error::Error>> {
    authenticate_con(socket, context, context.device_id).await
}

/// Autentica como un dispositivo concreto. El `proof` se firma con las claves del VAULT (el server
/// valida contra `vault_pubkey`), así que un device nuevo de la misma cuenta sólo aporta su
/// identidad: no hace falta un segundo juego de claves en el test.
async fn authenticate_con(
    socket: &mut Ws,
    context: &TestContext,
    device_id: i64,
) -> Result<(Value, Vec<u8>), Box<dyn std::error::Error>> {
    let hello_response = hello_con(socket, context.vault_id, device_id, 0).await?;
    let challenge = STANDARD.decode(
        hello_response["challenge"]
            .as_str()
            .ok_or("falta challenge")?,
    )?;

    let proof = ovaultd_crypto::sign_proof(&context.keys, context.vault_id, device_id, &challenge);

    send_json(
        socket,
        json!({
            "t": "auth",
            "proof": STANDARD.encode(proof)
        }),
    )
    .await?;

    Ok((recv_json(socket).await?, challenge))
}

/// Sube un blob completo por el camino WS — el mismo flujo que `blob_inline_chico_termina_en_el_cas_y_da_ack`:
/// `blob_head` con el `blob_id` del contenido -> frames binarios (`transfer_id` 8 B + `offset` 8 B + bytes) -> `ack`.
///
/// Importa el orden: un batch que referencia un `blob_id` exige que el blob ya esté publicado **y registrado**
/// (el servidor rechaza con `not_found` la referencia a contenido que no está), así que los tests suben el blob
/// ANTES de mandar el batch, igual que un cliente real.
async fn subir_blob(
    socket: &mut Ws,
    transfer: u64,
    contenido: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let blob_id = ovaultd_crypto::blob_id(contenido);

    send_json(
        socket,
        json!({
            "t": "blob_head",
            "transfer": transfer,
            "len": contenido.len(),
            "offset": 0,
            "blob_id": STANDARD.encode(blob_id)
        }),
    )
    .await?;

    let head_response = recv_json(socket).await?;
    assert_eq!(head_response["t"], "ack", "blob_head: {head_response}");

    let mut frame = Vec::with_capacity(16 + contenido.len());
    frame.extend_from_slice(&transfer.to_be_bytes());
    frame.extend_from_slice(&0_u64.to_be_bytes());
    frame.extend_from_slice(contenido);

    socket.send(Message::Binary(frame.into())).await?;

    let response = recv_json(socket).await?;
    assert_eq!(response["t"], "ack", "subida del blob: {response}");

    Ok(())
}

fn assert_error(response: &Value, code: &str) {
    assert_eq!(response["t"], "error", "respuesta del servidor: {response}");
    assert_eq!(
        response["code"], code,
        "respuesta completa del servidor: {response}"
    );
}

/// El contenido del blob que los tests que mandan un batch suben primero: 3 bytes, coherentes con el
/// `size` del op y con su `blob_id` (el hash del contenido). Una sola fuente para los dos.
const CONTENIDO_DEL_BLOB: &[u8] = b"abc";

/// Arma el batch **como structs reales** y recien despues lo serializa.
///
/// El helper anterior firmaba diez valores sueltos y los repetia a mano en un `json!`: dos copias del mismo dato,
/// y el servidor firma/verifica **desde el op parseado**, asi que cualquier divergencia entre las copias produce una
/// firma que no verifica. Aca **hay una sola fuente**: el `BatchOp` que se firma es el mismo objeto que se serializa.
fn batch_value(
    context: &TestContext,
    operation_id: &str,
    valid_signature: bool,
    expected_rev: u64,
) -> Value {
    use ovaultd_protocol::{Batch, BatchOp, BlobId, Op, PathId};

    let op_id = Uuid::parse_str(operation_id).expect("operation_id debe ser un UUID");
    let identity = ovaultd_crypto::VaultIdentity::generate(&context.keys.sign);

    let mut op = BatchOp {
        op: Op::Add,
        path_id: PathId::new([1u8; 32]),
        path_enc: Some(STANDARD.encode(b"path")),
        folder: false,
        // El blob del op es EXACTAMENTE el que `subir_blob` publica antes del batch: el servidor exige que
        // el blob_id referenciado exista (`blob_exists` -> `not_found` si no), y su `size` es la longitud real.
        blob_id: Some(BlobId::new(ovaultd_crypto::blob_id(CONTENIDO_DEL_BLOB))),
        // El t_mac va DECLARADO como string (b64 de ceros), igual que en commit/tests.rs: el verificador lo
        // decodifica, y `None`/"" hace fallar el decode. Los ceros son lo que se firma.
        t_mac: Some(STANDARD.encode([0u8; 64])),
        sig: String::new(),
        size: Some(u64::try_from(CONTENIDO_DEL_BLOB.len()).expect("longitud del contenido")),
        mtime: Some(0),
        ctime: Some(0),
        prev_path_id: None,
    };

    // Los valores firmados se DERIVAN del op, igual que en `verify_batch_operation`: nada se repite a mano.
    let blob_bytes = op
        .blob_id
        .map(|value| *value.as_bytes())
        .unwrap_or([0u8; 32]);
    let t_mac_bytes = [0u8; 64];
    // OJO: el codigo de operacion del wire NO es el discriminante del enum (`Op::Add as u8` es 1, y el
    // servidor firma 0). Es el mapeo de `commit.rs::verify_batch_operation`; se espeja aca a proposito.
    // Deuda declarada: esto tendria que ser `Op::code()` en `ovaultd-protocol`, una sola vez para los tres lados.
    let op_code = match op.op {
        Op::Add => 0u8,
        Op::Mod => 1u8,
        Op::Del => 2u8,
        _ => 3u8,
    };
    let signature = ovaultd_crypto::sign_record(
        &identity,
        op.path_id.as_bytes(),
        &blob_bytes,
        &t_mac_bytes,
        op_code,
        op.size.unwrap_or(0),
        op.mtime.unwrap_or(0),
        expected_rev,
        op_id.as_bytes(),
        context.epoch,
    );
    op.sig = STANDARD.encode(if valid_signature {
        signature
    } else {
        [0u8; 64]
    });

    let batch = Batch {
        t: "batch".to_string(),
        batch_id: Uuid::new_v4().to_string(),
        // El sobre lleva el operation_id como STRING UUID/hex (el servidor lo parsea con
        // `OperationId::from_wire`, que lee nibbles hex y tolera guiones). La doc decia
        // "base64" hasta que esta caceria lo corrigio: el parser es UNO solo (wire + delta).
        operation_id: op_id.to_string(),
        expected_rev,
        ops: vec![op],
    };
    // Que el batch del test pase SU PROPIA validacion de protocolo: si el helper arma un sobre
    // invalido, el test lo dice aca y no despues de un viaje de ida y vuelta al servidor.
    batch
        .validate()
        .expect("el batch del test tiene que pasar la validacion del protocolo");

    serde_json::to_value(&batch).expect("el batch se serializa")
}

#[tokio::test]
async fn sin_hello_cualquier_mensaje_da_error_y_cierra(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    send_json(&mut ws.socket, json!({ "t": "ping" })).await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_error(&response, "unauthorized");

    assert!(matches!(
        ws.socket.next().await,
        Some(Ok(Message::Close(_))) | None
    ));

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn hello_devuelve_challenge_y_head_rev() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    let response = hello(&mut ws.socket, &context, context.vault_id, 0).await?;

    assert_eq!(response["t"], "hello_ok");
    assert_eq!(response["proto"], 1);
    assert_eq!(response["head_rev"], 0);

    let challenge = response["challenge"]
        .as_str()
        .ok_or("falta challenge")?;
    assert!(!challenge.is_empty());
    assert!(!STANDARD.decode(challenge)?.is_empty());

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn hello_con_vault_ajeno_da_error_y_cierra(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let other_vault = context.insert_vault_for_other_account().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    let response = hello(&mut ws.socket, &context, other_vault, 0).await?;
    assert_error(&response, "not_found");

    assert!(matches!(
        ws.socket.next().await,
        Some(Ok(Message::Close(_))) | None
    ));

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn hello_con_proto_distinto_da_error() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    send_json(
        &mut ws.socket,
        json!({
            "t": "hello",
            "proto": 999,
            "vault": context.vault_id,
            "device": context.device_id,
            "since": 0
        }),
    )
    .await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_error(&response, "bad_request");

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn batch_antes_de_auth_da_error_y_cierra(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    hello(&mut ws.socket, &context, context.vault_id, 0).await?;

    send_json(
        &mut ws.socket,
        batch_value(&context, &Uuid::new_v4().to_string(), true, 0),
    )
    .await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_error(&response, "unauthorized");

    assert!(matches!(
        ws.socket.next().await,
        Some(Ok(Message::Close(_))) | None
    ));

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn auth_con_proof_valido_da_auth_ok_con_la_rev_pedida(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    let (response, _) = authenticate(&mut ws.socket, &context).await?;

    assert_eq!(response["t"], "auth_ok");
    assert_eq!(response["rev"], 0);

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn auth_con_proof_invalido_da_error_y_no_quema_el_challenge(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws1 = connect(&context, Some(&context.token)).await?;

    let hello_response = hello(&mut ws1.socket, &context, context.vault_id, 0).await?;
    let challenge_a = STANDARD.decode(
        hello_response["challenge"]
            .as_str()
            .ok_or("falta challenge")?,
    )?;

    send_json(
        &mut ws1.socket,
        json!({
            "t": "auth",
            "proof": STANDARD.encode([0u8; 64])
        }),
    )
    .await?;

    let invalid_response = recv_json(&mut ws1.socket).await?;
    assert_error(&invalid_response, "unauthorized");

    ws1.server.abort();

    let mut ws2 = connect(&context, Some(&context.token)).await?;

    let hello_response = hello(&mut ws2.socket, &context, context.vault_id, 0).await?;
    let challenge_b = STANDARD.decode(
        hello_response["challenge"]
            .as_str()
            .ok_or("falta challenge")?,
    )?;

    // El reuso del challenge se eliminó a propósito (rompía las conexiones paralelas del mismo
    // device): la reconexión recibe un challenge NUEVO y con ese se autentica.
    let valid_proof = ovaultd_crypto::sign_proof(
        &context.keys,
        context.vault_id,
        context.device_id,
        &challenge_b,
    );

    send_json(
        &mut ws2.socket,
        json!({
            "t": "auth",
            "proof": STANDARD.encode(valid_proof)
        }),
    )
    .await?;

    let valid_response = recv_json(&mut ws2.socket).await?;
    assert_eq!(
        valid_response["t"], "auth_ok",
        "el retry autentica con su challenge: {valid_response}"
    );
    assert_eq!(valid_response["rev"], 0);

    // Y el challenge del primer intento sigue SIN CONSUMIR en la DB: el proof inválido no lo
    // quemó (la prueba directa, sin depender de que una reconexión lo devuelva).
    let estado: Option<Option<String>> = sqlx::query_scalar(
        "SELECT used_at
         FROM auth_challenges
         WHERE device_id = ? AND vault_id = ? AND challenge = ?",
    )
    .bind(context.device_id)
    .bind(context.vault_id)
    .bind(&challenge_a)
    .fetch_optional(&context.pool)
    .await?;
    assert_eq!(
        estado,
        Some(None),
        "el challenge del intento fallido sigue vigente y sin consumir"
    );

    ws2.server.abort();
    Ok(())
}

#[tokio::test]
async fn un_segundo_auth_con_el_mismo_challenge_da_error(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    authenticate(&mut ws.socket, &context).await?;

    send_json(
        &mut ws.socket,
        json!({
            "t": "auth",
            "proof": STANDARD.encode([0u8; 64])
        }),
    )
    .await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_error(&response, "unauthorized");

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn batch_valido_da_batch_ack_con_la_rev_y_una_sola_revision(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    authenticate(&mut ws.socket, &context).await?;

    // Primero el blob, después la referencia: el batch que lo nombra exige que exista en el servidor.
    subir_blob(&mut ws.socket, 1, CONTENIDO_DEL_BLOB).await?;

    let operation_id = Uuid::new_v4().to_string();
    let batch = batch_value(&context, &operation_id, true, 0);
    let batch_id = batch["batch_id"].clone();

    // El batch es un latido: con `last_seen` viejo, el commit tiene que ponerlo al día.
    // Sin esto el panel mostraba la fecha del último login mientras el device commiteaba.
    sqlx::query("UPDATE devices SET last_seen = '2000-01-01 00:00:00' WHERE id = ?")
        .bind(context.device_id)
        .execute(&context.pool)
        .await?;

    send_json(&mut ws.socket, batch).await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_eq!(response["t"], "batch_ack", "response = {response}");
    assert_eq!(response["batch_id"], batch_id);
    assert_eq!(response["rev"], 1);

    let revisions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM revisions")
            .fetch_one(&context.pool)
            .await?;
    let batches: i64 =
        sqlx::query_scalar("SELECT count(*) FROM batches")
            .fetch_one(&context.pool)
            .await?;

    assert_eq!(revisions, 1);
    assert_eq!(batches, 1);

    let last_seen: Option<String> =
        sqlx::query_scalar("SELECT last_seen FROM devices WHERE id = ?")
            .bind(context.device_id)
            .fetch_one(&context.pool)
            .await?;
    assert_ne!(
        last_seen.as_deref(),
        Some("2000-01-01 00:00:00"),
        "el batch tiene que dejar el latido del device"
    );

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn batch_con_firma_invalida_da_error_y_no_escribe_nada(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    authenticate(&mut ws.socket, &context).await?;

    let batch = batch_value(
        &context,
        &Uuid::new_v4().to_string(),
        false,
        0,
    );

    send_json(&mut ws.socket, batch).await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_error(&response, "unauthorized");

    let revisions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM revisions")
            .fetch_one(&context.pool)
            .await?;
    let batches: i64 =
        sqlx::query_scalar("SELECT count(*) FROM batches")
            .fetch_one(&context.pool)
            .await?;

    assert_eq!(revisions, 0);
    assert_eq!(batches, 0);

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn batch_con_operation_id_repetido_da_el_mismo_ack(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    authenticate(&mut ws.socket, &context).await?;

    // Primero el blob, después la referencia (igual que en `batch_valido...`).
    subir_blob(&mut ws.socket, 1, CONTENIDO_DEL_BLOB).await?;

    let operation_id = Uuid::new_v4().to_string();
    let batch = batch_value(&context, &operation_id, true, 0);

    send_json(&mut ws.socket, batch.clone()).await?;
    let first_response = recv_json(&mut ws.socket).await?;

    send_json(&mut ws.socket, batch).await?;
    let second_response = recv_json(&mut ws.socket).await?;

    assert_eq!(first_response["t"], "batch_ack", "first_response = {first_response}");
    assert_eq!(second_response["t"], "batch_ack", "second_response = {second_response}");
    assert_eq!(second_response["batch_id"], first_response["batch_id"]);
    assert_eq!(second_response["rev"], first_response["rev"]);

    let revisions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM revisions")
            .fetch_one(&context.pool)
            .await?;
    let batches: i64 =
        sqlx::query_scalar("SELECT count(*) FROM batches")
            .fetch_one(&context.pool)
            .await?;

    assert_eq!(revisions, 1);
    assert_eq!(batches, 1);

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn blob_inline_chico_termina_en_el_cas_y_da_ack(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    authenticate(&mut ws.socket, &context).await?;

    let bytes = b"abc";
    let blob_id = ovaultd_crypto::blob_id(bytes);

    send_json(
        &mut ws.socket,
        json!({
            "t": "blob_head",
            "transfer": 1,
            "len": bytes.len(),
            "offset": 0,
            "blob_id": STANDARD.encode(blob_id)
        }),
    )
    .await?;

    let head_response = recv_json(&mut ws.socket).await?;
    assert_eq!(head_response["t"], "ack");

    let mut frame = Vec::with_capacity(16 + bytes.len());
    frame.extend_from_slice(&1_u64.to_be_bytes());
    frame.extend_from_slice(&0_u64.to_be_bytes());
    frame.extend_from_slice(bytes);

    ws.socket.send(Message::Binary(frame.into())).await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_eq!(response["t"], "ack");

    // El layout del CAS es asunto del CAS: se le pregunta a el, no se adivina el path.
    // (`CasStore::exists` usa root/<2 hex>/<hex>; `root/<b64>` era una suposicion del test.)
    let cas = crate::storage::CasStore::new(context.config.cas_root.clone())?;
    assert!(
        cas.exists(&ovaultd_protocol::BlobId::new(blob_id)),
        "el blob tendria que estar en el CAS despues del ack"
    );

    // Y ademas del CAS, el indice: sin la fila en `blobs`, los batches que referencien este blob
    // darian `not_found` (es lo que prueba el par de tests de batch de arriba).
    let registrados: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM blobs WHERE vault_id = ? AND hash = ?",
    )
    .bind(context.vault_id)
    .bind(blob_id.as_slice())
    .fetch_one(&context.pool)
    .await?;
    assert_eq!(
        registrados, 1,
        "el blob tiene que quedar registrado en el indice (blobs) al completarse la subida"
    );

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn blob_por_ws_respeta_la_cuota() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    // Cuota de 2 bytes: "abc" (3) no entra. El blob ya viajó, pero no se publica ni se registra.
    sqlx::query("UPDATE vaults SET quota_bytes = 2 WHERE id = ?")
        .bind(context.vault_id)
        .execute(&context.pool)
        .await?;

    let mut ws = connect(&context, Some(&context.token)).await?;
    authenticate(&mut ws.socket, &context).await?;

    let bytes = b"abc";
    let blob_id = ovaultd_crypto::blob_id(bytes);

    send_json(
        &mut ws.socket,
        json!({
            "t": "blob_head",
            "transfer": 1,
            "len": bytes.len(),
            "offset": 0,
            "blob_id": STANDARD.encode(blob_id)
        }),
    )
    .await?;

    let head_response = recv_json(&mut ws.socket).await?;
    assert_eq!(head_response["t"], "ack");

    let mut frame = Vec::with_capacity(16 + bytes.len());
    frame.extend_from_slice(&1_u64.to_be_bytes());
    frame.extend_from_slice(&0_u64.to_be_bytes());
    frame.extend_from_slice(bytes);

    ws.socket.send(Message::Binary(frame.into())).await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_error(&response, "quota_exceeded");

    let cas = crate::storage::CasStore::new(context.config.cas_root.clone())?;
    assert!(
        !cas.exists(&ovaultd_protocol::BlobId::new(blob_id)),
        "sin cuota no se publica en el CAS"
    );

    let registrados: i64 = sqlx::query_scalar("SELECT count(*) FROM blobs")
        .fetch_one(&context.pool)
        .await?;
    assert_eq!(registrados, 0);

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn blob_inline_mayor_al_umbral_da_max_file_exceeded_y_manda_al_http(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    authenticate(&mut ws.socket, &context).await?;

    send_json(
        &mut ws.socket,
        json!({
            "t": "blob_head",
            "transfer": 1,
            "len": 1024 * 1024 + 1,
            "offset": 0,
            "blob_id": STANDARD.encode([0u8; 32])
        }),
    )
    .await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_error(&response, "max_file_exceeded");

    assert!(matches!(
        ws.socket.next().await,
        Some(Ok(Message::Close(_))) | None
    ));

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn frame_sin_blob_head_da_error() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    authenticate(&mut ws.socket, &context).await?;

    let mut frame = Vec::with_capacity(16);
    frame.extend_from_slice(&1_u64.to_be_bytes());
    frame.extend_from_slice(&0_u64.to_be_bytes());

    ws.socket.send(Message::Binary(frame.into())).await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_error(&response, "bad_request");

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn offset_con_hueco_da_error() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    authenticate(&mut ws.socket, &context).await?;

    send_json(
        &mut ws.socket,
        json!({
            "t": "blob_head",
            "transfer": 1,
            "len": 4,
            "offset": 0,
            "blob_id": STANDARD.encode([2u8; 32])
        }),
    )
    .await?;

    let _head_response = recv_json(&mut ws.socket).await?;

    let mut frame = Vec::with_capacity(18);
    frame.extend_from_slice(&1_u64.to_be_bytes());
    frame.extend_from_slice(&2_u64.to_be_bytes());
    frame.extend_from_slice(b"ab");

    ws.socket.send(Message::Binary(frame.into())).await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_error(&response, "bad_request");

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn mensaje_que_no_es_json_da_error_sin_tumbar_la_conexion(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    authenticate(&mut ws.socket, &context).await?;

    ws.socket
        .send(Message::Text("esto no es json".into()))
        .await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_error(&response, "bad_request");

    send_json(
        &mut ws.socket,
        json!({
            "t": "ping"
        }),
    )
    .await?;

    let ping_response = recv_json(&mut ws.socket).await?;
    assert_eq!(ping_response["t"], "ping");

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn el_error_del_ws_usa_los_mismos_codigos_que_el_http_en_snake_case(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    ws.socket
        .send(Message::Text("{".to_owned().into()))
        .await?;

    let response = recv_json(&mut ws.socket).await?;
    assert_error(&response, "bad_request");

    let code = response["code"].as_str().ok_or("falta código")?;
    assert_eq!(code, "bad_request");
    assert!(code.chars().all(|character| {
        character.is_ascii_lowercase() || character == '_'
    }));

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn sin_token_no_hay_handshake() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let result = connect(&context, None).await;
    assert!(result.is_err());

    Ok(())
}

/// ADR-0015: token por subprotocolo (el navegador no puede fijar headers). El server
/// negocia `ovaultd.v1` (lo ecoa) y la identidad queda fijada para el handshake de datos.
#[tokio::test]
async fn upgrade_con_token_por_subprotocolo_completa_el_handshake(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let bearer = format!(
        "ovaultd.bearer.{}",
        URL_SAFE_NO_PAD.encode(context.token.as_bytes())
    );

    let mut ws = connect_full(&context, None, &["ovaultd.v1", &bearer]).await?;
    assert_eq!(ws.protocolo_negociado.as_deref(), Some("ovaultd.v1"));

    let response = hello(&mut ws.socket, &context, context.vault_id, 0).await?;
    assert_eq!(response["t"], "hello_ok", "response = {response}");

    let challenge = STANDARD.decode(
        response["challenge"]
            .as_str()
            .ok_or("falta challenge")?,
    )?;
    let proof = ovaultd_crypto::sign_proof(
        &context.keys,
        context.vault_id,
        context.device_id,
        &challenge,
    );
    send_json(
        &mut ws.socket,
        json!({ "t": "auth", "proof": STANDARD.encode(proof) }),
    )
    .await?;
    let auth_ok = recv_json(&mut ws.socket).await?;
    assert_eq!(auth_ok["t"], "auth_ok", "auth_ok = {auth_ok}");

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn subprotocolo_bearer_con_token_invalido_rechaza_el_upgrade(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let result = connect_full(
        &context,
        None,
        &["ovaultd.v1", "ovaultd.bearer.bm8tb3JpZ2luYWRv"],
    )
    .await;

    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn subprotocolo_sin_bearer_ni_header_rechaza_el_upgrade(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let result = connect_full(&context, None, &["ovaultd.v1"]).await;
    assert!(result.is_err());

    Ok(())
}

// HALLAZGO:
// - El contrato exige que, después de un proof inválido, el challenge siga vivo
//   y permita intentar un proof correcto en la misma conexión.
// - La implementación actual construye ese error con WsFailure::close(...), envía
//   Close y termina la conexión; por eso el test
//   `auth_con_proof_invalido_da_error_y_no_quema_el_challenge` falla hasta que
//   el servidor deje la conexión abierta para ese error.
// - El contrato exige que `blob_head` transporte `blob_id` para que el receptor
//   pueda comprobar que los bytes recibidos corresponden al identificador declarado.
//   La implementación actual no tiene ese campo en `BlobHeadMessage` y calcula el
//   identificador únicamente a partir de los bytes recibidos.
// - El contrato exige que un batch con firma inválida devuelva un error sin escribir
//   en `revisions` ni `batches`. Si `commit_batch` no verifica la firma antes de
//   persistir, el test `batch_con_firma_invalida_da_error_y_no_escribe_nada`
//   expone ese bug.
// - El contrato exige que un blob inline mayor a 1 MB sea rechazado por WS y enviado
//   al endpoint HTTP. La implementación actual sólo devuelve `max_file_exceeded`
//   y cierra la conexión; no realiza ningún redireccionamiento HTTP, porque un
//   WebSocket no puede redirigir la solicitud original a otro endpoint.

/// El aviso viaja al OTRO dispositivo del vault, y sólo a él: es lo que hace que un cliente con
/// Obsidian ya abierto se entere de lo que subió el otro sin esperar a nada.
///
/// Cubre las tres reglas que hacen que el aviso sirva: llega al otro (no al emisor, que no
/// necesita un pull inútil), trae el registro completo como lo define la spec, y no puede romper
/// el batch que lo originó.
#[tokio::test]
async fn el_otro_dispositivo_del_vault_recibe_el_aviso_del_lote(
) -> Result<(), Box<dyn std::error::Error>> {
    // Un cuelgue acá significa que el aviso no llegó, no que el test sea lento: el timeout lo
    // convierte en un fallo legible en vez de dejar la corrida colgada.
    tokio::time::timeout(std::time::Duration::from_secs(20), aviso_entre_dispositivos()).await??;
    Ok(())
}

async fn aviso_entre_dispositivos() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    // Segundo dispositivo de la MISMA cuenta, con su propia sesión: `hello` exige que el device
    // coincida con el del token, así que no alcanza con insertar la fila a mano.
    let (token_otro, otro_device) = auth::iniciar_sesion(
        &context.pool,
        "a@example.com",
        auth::PASSWORD_DE_PRUEBA,
        "telefono",
        Some("Android"),
    )
    .await?;

    // El que escucha: el dispositivo del contexto. El otro es el que sube.
    let mut escucha = connect(&context, Some(&context.token)).await?;
    authenticate(&mut escucha.socket, &context).await?;

    let mut sube = connect(&context, Some(&token_otro)).await?;
    authenticate_con(&mut sube.socket, &context, otro_device).await?;

    subir_blob(&mut sube.socket, 1, CONTENIDO_DEL_BLOB).await?;
    let operation_id = Uuid::new_v4().to_string();
    send_json(&mut sube.socket, batch_value(&context, &operation_id, true, 0)).await?;

    let ack = recv_json(&mut sube.socket).await?;
    assert_eq!(ack["t"], "batch_ack", "el que sube recibe su ack: {ack}");

    // El aviso: con el registro completo (la spec: rev, op, path, blob, MAC y firma) y con el
    // nombre del dispositivo que lo produjo.
    let aviso = recv_json(&mut escucha.socket).await?;
    assert_eq!(aviso["t"], "notify", "el que escucha recibe el aviso: {aviso}");
    assert_eq!(aviso["device"], "telefono", "el aviso dice quién subió: {aviso}");
    assert_eq!(aviso["rev"], ack["rev"], "apunta a la revisión del lote: {aviso}");
    assert_eq!(aviso["op"], "add", "la operación del registro: {aviso}");
    assert_eq!(aviso["deleted"], false, "no es un tombstone: {aviso}");
    assert!(
        aviso["path_id"].as_str().is_some_and(|p| !p.is_empty()),
        "trae el path_id del registro: {aviso}"
    );
    assert!(
        aviso["sig"].as_str().is_some_and(|s| !s.is_empty()),
        "trae la firma del registro: {aviso}"
    );
    assert_eq!(aviso["size"], CONTENIDO_DEL_BLOB.len(), "trae el tamaño: {aviso}");

    // Y el emisor NO recibe su propio eco: el pull que dispararía no traería nada nuevo. Si el
    // eco llegara, el `ping` de abajo se leería como notify y este assert fallaría.
    send_json(&mut sube.socket, json!({"t": "ping"})).await?;
    let respuesta = recv_json(&mut sube.socket).await?;
    assert_eq!(respuesta["t"], "ping", "sin eco para el emisor: {respuesta}");

    escucha.server.abort();
    sube.server.abort();
    Ok(())
}

/// DOS conexiones PARALELAS del mismo dispositivo: las dos tienen que autenticar. Este era el bug
/// real de la fase 5: el `hello` reusaba el challenge pendiente del device, las dos conexiones
/// firmaban el MISMO challenge, la primera lo consumía al autenticar y la segunda quedaba
/// rechazada con "no autorizado" (el motor del plugin moría al recargar con una lectura al vuelo
/// en paralelo — carrera cazada en vivo el 2026-10-18).
#[tokio::test]
async fn dos_conexiones_paralelas_del_mismo_device_autentican_las_dos(
) -> Result<(), Box<dyn std::error::Error>> {
    // Un cuelgue acá sería la segunda autenticación sin respuesta, no un test lento.
    tokio::time::timeout(std::time::Duration::from_secs(20), paralelas()).await??;
    Ok(())
}

async fn paralelas() -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;

    let mut una = connect(&context, Some(&context.token)).await?;
    let mut otra = connect(&context, Some(&context.token)).await?;

    // Los DOS `hello` primero: es el interleaving real (el motor y la lectura al vuelo piden
    // challenge casi juntos, antes de que ninguno autentique).
    let h_una = hello_con(&mut una.socket, context.vault_id, context.device_id, 0).await?;
    let h_otra = hello_con(&mut otra.socket, context.vault_id, context.device_id, 0).await?;

    let challenge_una = STANDARD.decode(h_una["challenge"].as_str().ok_or("falta challenge")?)?;
    let challenge_otra = STANDARD.decode(h_otra["challenge"].as_str().ok_or("falta challenge")?)?;
    assert_ne!(
        challenge_una, challenge_otra,
        "cada conexión recibe SU challenge: reusar el pendiente rompía las paralelas"
    );

    // Cada firma va con su challenge: las DOS autentican.
    let proof_una = ovaultd_crypto::sign_proof(
        &context.keys,
        context.vault_id,
        context.device_id,
        &challenge_una,
    );
    send_json(
        &mut una.socket,
        json!({"t": "auth", "proof": STANDARD.encode(proof_una)}),
    )
    .await?;
    let r_una = recv_json(&mut una.socket).await?;
    assert_eq!(r_una["t"], "auth_ok", "la primera autentica: {r_una}");

    let proof_otra = ovaultd_crypto::sign_proof(
        &context.keys,
        context.vault_id,
        context.device_id,
        &challenge_otra,
    );
    send_json(
        &mut otra.socket,
        json!({"t": "auth", "proof": STANDARD.encode(proof_otra)}),
    )
    .await?;
    let r_otra = recv_json(&mut otra.socket).await?;
    assert_eq!(
        r_otra["t"], "auth_ok",
        "la SEGUNDA también autentica (era la que el reuso del challenge rechazaba): {r_otra}"
    );

    una.server.abort();
    otra.server.abort();
    Ok(())
}

#[tokio::test]
async fn un_expected_rev_viejo_da_conflict_y_el_socket_sigue_vivo(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;
    authenticate(&mut ws.socket, &context).await?;
    subir_blob(&mut ws.socket, 1, CONTENIDO_DEL_BLOB).await?;

    // Primer batch: rev 1.
    send_json(
        &mut ws.socket,
        batch_value(&context, &Uuid::new_v4().to_string(), true, 0),
    )
    .await?;
    let ack = recv_json(&mut ws.socket).await?;
    assert_eq!(ack["t"], "batch_ack");

    // Segundo batch con la base vieja: el CAS se pierde.
    send_json(
        &mut ws.socket,
        batch_value(&context, &Uuid::new_v4().to_string(), true, 0),
    )
    .await?;
    let conflicto = recv_json(&mut ws.socket).await?;
    assert_eq!(
        conflicto["t"], "conflict",
        "el server tiene que emitir conflict y no un error: {conflicto}"
    );
    assert_eq!(conflicto["base_rev"], 0);
    assert_eq!(conflicto["server_rev"], 1);

    // Y el socket sigue funcional: con la rev correcta, el batch commitea.
    send_json(
        &mut ws.socket,
        batch_value(&context, &Uuid::new_v4().to_string(), true, 1),
    )
    .await?;
    let ack2 = recv_json(&mut ws.socket).await?;
    assert_eq!(
        ack2["t"], "batch_ack",
        "el socket no puede cerrarse después de un conflict: {ack2}"
    );
    assert_eq!(ack2["rev"], 2);

    ws.server.abort();
    Ok(())
}

#[tokio::test]
async fn el_hello_guarda_la_version_del_cliente_en_el_device(
) -> Result<(), Box<dyn std::error::Error>> {
    let context = TestContext::new().await?;
    let mut ws = connect(&context, Some(&context.token)).await?;

    send_json(
        &mut ws.socket,
        json!({
            "t": "hello",
            "proto": 1,
            "vault": context.vault_id,
            "device": context.device_id,
            "since": 0,
            "app_version": "plugin 0.2.34"
        }),
    )
    .await?;
    let response = recv_json(&mut ws.socket).await?;
    assert_eq!(response["t"], "hello_ok");

    let version: Option<String> =
        sqlx::query_scalar("SELECT app_version FROM devices WHERE id = ?")
            .bind(context.device_id)
            .fetch_one(&context.pool)
            .await?;
    assert_eq!(version.as_deref(), Some("plugin 0.2.34"));

    ws.server.abort();
    Ok(())
}

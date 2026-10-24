use futures::{SinkExt, StreamExt};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::broadcast;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header::AUTHORIZATION, HeaderMap},
    response::Response,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use ovaultd_protocol::{Batch, BlobId, ErrorCode, Op, PathId, SignedRecord};

use crate::{
    auth,
    error::{Result, ServerError},
    routes::{autenticar_token, resolve_vault, EstadoServidor},
    storage::CasStore,
    sync::commit::commit_batch,
};

const PROTOCOL_VERSION: u64 = 1;

/// Inactividad máxima sin un solo frame del cliente: reconecta solo y retoma por el oplog;
/// una conexión idle para siempre ocupa un fd y una tarea.
const INACTIVIDAD: std::time::Duration = std::time::Duration::from_secs(300);
/// Transferencias simultáneas por conexión: cada `blob_head` reserva `len` bytes.
const MAX_TRANSFERS: usize = 4;
const MAX_INLINE_BLOB_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ClientMessage {
    Hello(HelloMessage),
    Auth(AuthMessage),
    Batch(BatchMessage),
    BlobHead(BlobHeadMessage),
    Ping,
    Pull(PullMessage),
}

#[derive(Debug, Deserialize)]
struct HelloMessage {
    proto: u64,
    vault: i64,
    device: i64,
    since: u64,
}

#[derive(Debug, Deserialize)]
struct AuthMessage {
    proof: String,
}

#[derive(Debug, Deserialize)]
struct BatchMessage {
    #[serde(flatten)]
    batch: Batch,
}

#[derive(Debug, Deserialize)]
struct PullMessage {
    since: u64,
}

#[derive(Debug, Deserialize)]
struct BlobHeadMessage {
    transfer: u64,
    blob_id: ovaultd_protocol::BlobId,
    len: u64,
    offset: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ServerMessage {
    HelloOk {
        proto: u64,
        challenge: String,
        head_rev: u64,
    },
    AuthOk {
        rev: u64,
    },
    BatchAck {
        batch_id: String,
        rev: u64,
    },
    PullAck {
        rev: u64,
        ok: bool,
    },
    Ack {
        rev: u64,
    },
    Error {
        code: String,
        detail: String,
    },
    Ping,
    /// `notify`: "hay revisiones nuevas en este vault". Lleva el registro completo, como lo define
    /// la spec, y quien lo recibe hace un pull: el aviso NO es la fuente de verdad, es el empujón.
    Notify {
        rev: u64,
        op: Op,
        path_id: PathId,
        blob_id: BlobId,
        t_mac: String,
        sig: String,
        size: u64,
        device: String,
        deleted: bool,
        prev_path_id: Option<PathId>,
    },
}

struct BlobTransfer {
    expected_len: usize,
    next_offset: usize,
    blob_id: ovaultd_protocol::BlobId,
    bytes: Vec<u8>,
}

struct Connection {
    pool: SqlitePool,
    cas: Arc<CasStore>,
    account_id: Option<i64>,
    device_id: Option<i64>,
    vault_id: Option<i64>,
    challenge: Option<Vec<u8>>,
    since: u64,
    authenticated: bool,
    transfers: HashMap<u64, BlobTransfer>,
    /// Señal de "hay revisiones" del vault: la conexión se suscribe al autenticarse.
    notifier: Arc<crate::storage::Notifier>,
    avisos: Option<broadcast::Receiver<crate::storage::Aviso>>,
}

impl Connection {
    fn new(state: &EstadoServidor) -> Self {
        Self {
            pool: state.pool.clone(),
            cas: state.cas.clone(),
            account_id: None,
            device_id: None,
            vault_id: None,
            challenge: None,
            since: 0,
            authenticated: false,
            transfers: HashMap::new(),
            notifier: state.notifier.clone(),
            avisos: None,
        }
    }

    /// Se suscribe a los avisos del vault ya autenticado y entrega el receptor **una sola vez**:
    /// el loop del WebSocket lo toma y desde ahí escucha los dos canales a la vez.
    fn tomar_avisos(&mut self) -> Option<broadcast::Receiver<crate::storage::Aviso>> {
        self.avisos.take()
    }

    async fn handle_text(&mut self, text: &str) -> std::result::Result<ServerMessage, WsFailure> {
        let message: ClientMessage =
            serde_json::from_str(text).map_err(|error| {
                WsFailure::error(
                    ErrorCode::BadRequest,
                    format!("mensaje JSON inválido: {error}"),
                )
            })?;

        match message {
            ClientMessage::Hello(hello) => self.hello(hello).await,
            ClientMessage::Auth(auth) => self.auth(auth).await,
            ClientMessage::Batch(batch) => self.batch(batch).await,
            ClientMessage::BlobHead(head) => self.blob_head(head).await,
            ClientMessage::Ping => self.ping(),
            ClientMessage::Pull(pull) => self.pull(pull).await,
        }
    }

    async fn hello(
        &mut self,
        hello: HelloMessage,
    ) -> std::result::Result<ServerMessage, WsFailure> {
        if self.vault_id.is_some() {
            return Err(WsFailure::close(
                ErrorCode::BadRequest,
                "hello duplicado",
            ));
        }

        if hello.proto != PROTOCOL_VERSION {
            return Err(WsFailure::close(
                ErrorCode::BadRequest,
                format!(
                    "versión de protocolo no soportada: {}",
                    hello.proto
                ),
            ));
        }

        let account_id = self
            .account_id
            .ok_or_else(|| WsFailure::close(ErrorCode::Unauthorized, "dispositivo no autenticado"))?;

        let device_id = self
            .device_id
            .ok_or_else(|| WsFailure::close(ErrorCode::Unauthorized, "dispositivo no autenticado"))?;

        if hello.device != device_id {
            return Err(WsFailure::close(
                ErrorCode::Unauthorized,
                "el device del hello no es el dispositivo autenticado",
            ));
        }

        resolve_vault(&self.pool, account_id, hello.vault)
            .await
            .map_err(WsFailure::from_server_error)?;

        // SIEMPRE un challenge NUEVO: reusar el pendiente rompía las conexiones PARALELAS
        // del mismo device (el motor + una lectura al vuelo al recargar el plugin: las dos
        // recibían el mismo challenge, la primera lo consumía y la segunda quedaba
        // rechazada con "no autorizado" — carrera real del 2026-10-18). El cliente firma
        // el challenge que acaba de recibir, así que emitir uno nuevo no le cuesta nada.
        let challenge = auth::emitir_challenge(&self.pool, device_id, hello.vault)
            .await
            .map_err(WsFailure::from_server_error)?;

        let head_rev = sqlx::query_scalar::<_, i64>(
            "SELECT head_rev
             FROM vaults
             WHERE id = ? AND account_id = ?",
        )
        .bind(hello.vault)
        .bind(account_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(ServerError::from)
        .map_err(WsFailure::from_server_error)?
        .ok_or_else(|| WsFailure::close(ErrorCode::NotFound, "vault inexistente"))?;

        let head_rev = u64::try_from(head_rev)
            .map_err(|_| WsFailure::close(ErrorCode::Internal, "revisión inválida"))?;

        self.vault_id = Some(hello.vault);
        self.challenge = Some(challenge.clone());
        self.since = hello.since.min(head_rev);

        Ok(ServerMessage::HelloOk {
            proto: PROTOCOL_VERSION,
            challenge: STANDARD.encode(challenge),
            head_rev,
        })
    }

    async fn auth(
        &mut self,
        auth_message: AuthMessage,
    ) -> std::result::Result<ServerMessage, WsFailure> {
        let device_id = self
            .device_id
            .ok_or_else(|| WsFailure::close(ErrorCode::Unauthorized, "hello requerido"))?;
        let vault_id = self
            .vault_id
            .ok_or_else(|| WsFailure::close(ErrorCode::Unauthorized, "hello requerido"))?;
        let challenge = self
            .challenge
            .as_ref()
            .ok_or_else(|| WsFailure::close(ErrorCode::Unauthorized, "hello requerido"))?;

        if self.authenticated {
            return Err(WsFailure::close(
                ErrorCode::Unauthorized,
                "auth duplicado",
            ));
        }

        let proof_bytes = STANDARD
            .decode(auth_message.proof)
            .map_err(|_| WsFailure::close(ErrorCode::Unauthorized, "proof inválido"))?;

        let proof: [u8; 64] = proof_bytes
            .try_into()
            .map_err(|_| WsFailure::close(ErrorCode::Unauthorized, "proof inválido"))?;

        let public_key = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT vault_pubkey
             FROM vaults
             WHERE id = ?",
        )
        .bind(vault_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(ServerError::from)
        .map_err(WsFailure::from_server_error)?
        .ok_or_else(|| WsFailure::close(ErrorCode::NotFound, "vault inexistente"))?;

        let public_key: [u8; 32] = public_key
            .try_into()
            .map_err(|_| WsFailure::close(ErrorCode::Unauthorized, "clave pública inválida"))?;

        ovaultd_crypto::verify_proof(
            &public_key,
            vault_id,
            device_id,
            challenge,
            &proof,
        )
        .map_err(|_| WsFailure::close(ErrorCode::Unauthorized, "proof inválido"))?;

        auth::consumir_challenge(&self.pool, device_id, vault_id, challenge)
            .await
            .map_err(WsFailure::from_server_error)?;

        let head_rev = sqlx::query_scalar::<_, i64>(
            "SELECT head_rev
             FROM vaults
             WHERE id = ?",
        )
        .bind(vault_id)
        .fetch_one(&self.pool)
        .await
        .map_err(ServerError::from)
        .map_err(WsFailure::from_server_error)?;

        let head_rev = u64::try_from(head_rev)
            .map_err(|_| WsFailure::close(ErrorCode::Internal, "revisión inválida"))?;

        self.authenticated = true;
        // A partir de acá esta conexión escucha los avisos del vault: si otro dispositivo
        // commitea, el loop los reenvía y este cliente se entera sin esperar a nada.
        self.avisos = Some(self.notifier.suscribir(vault_id));
        self.since = self.since.min(head_rev);

        Ok(ServerMessage::AuthOk { rev: self.since })
    }

    async fn batch(
        &mut self,
        message: BatchMessage,
    ) -> std::result::Result<ServerMessage, WsFailure> {
        self.require_authenticated()?;

        let vault_id = self.vault_id.ok_or_else(|| {
            WsFailure::close(ErrorCode::Unauthorized, "vault no autenticado")
        })?;
        let device_id = self.device_id.ok_or_else(|| {
            WsFailure::close(ErrorCode::Unauthorized, "device no autenticado")
        })?;

        // El latido del device también va por acá: el sync real (pull por HTTP + push por
        // WS) NO pasa por `autenticar_token` en cada ciclo, así que sin esto `last_seen`
        // quedaba con la fecha del último login y el panel mentía ("hace 2 días" con un
        // device sincronizando cada 5 s). Throttle de 60 s: una escritura por minuto.
        let _ = sqlx::query(
            "UPDATE devices SET last_seen = datetime('now')
             WHERE id = ? AND (last_seen IS NULL OR last_seen < datetime('now', '-60 seconds'))",
        )
        .bind(device_id)
        .execute(&self.pool)
        .await;

        let outcome = commit_batch(&self.pool, vault_id, device_id, &message.batch)
            .await
            .map_err(WsFailure::from_server_error)?;

        // Sincronizar el CAS ANTES del ack: los blobs del batch tienen que estar en disco
        // (un blob confirmado no se re-sube nunca más). Es el ÚNICO fsync del camino de
        // subida: uno por lote (`syncfs`), no dos por blob. Medido: ~164 ms por lote.
        self.cas
            .sincronizar()
            .await
            .map_err(WsFailure::from_server_error)?;

        let rev = u64::try_from(outcome.rev)
            .map_err(|_| WsFailure::close(ErrorCode::Internal, "revisión inválida"))?;

        // El empujón para los demás dispositivos del vault. El batch YA commiteó acá: si el aviso
        // falla (o no hay nadie escuchando), no se pierde nada — el aviso es una señal, no una cola.
        self.avisar_del_lote(vault_id, device_id, outcome.rev).await;

        Ok(ServerMessage::BatchAck {
            batch_id: message.batch.batch_id.clone(),
            rev,
        })
    }

    /// Reenvía a los demás dispositivos del vault los registros que acaban de entrar. Los saca de
    /// la MISMA fuente que un pull (el oplog), no de una copia a mano: así el aviso nunca puede
    /// mentir sobre lo que hay, porque es literalmente lo que un pull devolvería.
    async fn avisar_del_lote(&self, vault_id: i64, device_id: i64, rev: i64) {
        let registros = match crate::delta::registros_de_rev(&self.pool, vault_id, rev).await {
            Ok(registros) => registros,
            Err(error) => {
                let _ = error;
                return;
            }
        };

        // La spec muestra el device legible ("laptop (Linux)"), así que va el nombre; si el
        // registro no está, el id cumple la misma función: es el filtro del eco.
        let device = sqlx::query_scalar::<_, String>("SELECT name FROM devices WHERE id = ?")
            .bind(device_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| device_id.to_string());

        let avisos: Vec<Arc<String>> = registros
            .iter()
            .filter_map(|registro| aviso_de_registro(registro, &device))
            .map(Arc::new)
            .collect();

        tracing::debug!(
            vault = vault_id,
            rev,
            registros = registros.len(),
            avisos = avisos.len(),
            device = %device,
            "aviso de lote"
        );

        if !avisos.is_empty() {
            self.notifier.avisar(vault_id, device_id, &avisos);
        }
    }

    async fn blob_head(
        &mut self,
        head: BlobHeadMessage,
    ) -> std::result::Result<ServerMessage, WsFailure> {
        self.require_authenticated()?;

        let len = usize::try_from(head.len)
            .map_err(|_| WsFailure::close(ErrorCode::MaxFileExceeded, "blob demasiado grande"))?;

        if len > MAX_INLINE_BLOB_BYTES {
            return Err(WsFailure::close(
                ErrorCode::MaxFileExceeded,
                "los blobs mayores a 1 MB deben subirse por HTTP mediante POST /api/v1/vaults/{id}/blobs",
            ));
        }

        let offset = usize::try_from(head.offset)
            .map_err(|_| WsFailure::close(ErrorCode::BadRequest, "offset inválido"))?;

        if offset > len {
            return Err(WsFailure::close(
                ErrorCode::BadRequest,
                "offset fuera del blob",
            ));
        }

        if self.transfers.contains_key(&head.transfer) {
            return Err(WsFailure::close(
                ErrorCode::BadRequest,
                "transfer ya abierto",
            ));
        }

        if self.transfers.len() >= MAX_TRANSFERS {
            return Err(WsFailure::close(
                ErrorCode::BadRequest,
                "demasiadas transferencias abiertas en esta conexión",
            ));
        }

        self.transfers.insert(
            head.transfer,
            BlobTransfer {
                expected_len: len,
                next_offset: offset,
                blob_id: head.blob_id,
                bytes: Vec::with_capacity(len),
            },
        );

        self.maybe_finish_blob(head.transfer).await
    }

    async fn handle_binary(
        &mut self,
        frame: &[u8],
    ) -> std::result::Result<Option<ServerMessage>, WsFailure> {
        self.require_authenticated()?;

        if frame.len() < 16 {
            return Err(WsFailure::close(
                ErrorCode::BadRequest,
                "frame binario demasiado corto",
            ));
        }

        let transfer = u64::from_be_bytes(
            frame[0..8]
                .try_into()
                .map_err(|_| WsFailure::close(ErrorCode::BadRequest, "transfer inválido"))?,
        );
        let offset = u64::from_be_bytes(
            frame[8..16]
                .try_into()
                .map_err(|_| WsFailure::close(ErrorCode::BadRequest, "offset inválido"))?,
        );

        let transfer_state = self.transfers.get_mut(&transfer).ok_or_else(|| {
            WsFailure::close(
                ErrorCode::BadRequest,
                "frame sin blob_head correspondiente",
            )
        })?;

        let offset = usize::try_from(offset)
            .map_err(|_| WsFailure::close(ErrorCode::BadRequest, "offset inválido"))?;

        if offset != transfer_state.next_offset {
            return Err(WsFailure::close(
                ErrorCode::BadRequest,
                "offset no consecutivo",
            ));
        }

        let payload = &frame[16..];

        if transfer_state.next_offset + payload.len() > transfer_state.expected_len {
            return Err(WsFailure::close(
                ErrorCode::BadRequest,
                "frame excede el tamaño declarado",
            ));
        }

        transfer_state.bytes.extend_from_slice(payload);
        transfer_state.next_offset += payload.len();

        if transfer_state.next_offset == transfer_state.expected_len {
            self.maybe_finish_blob(transfer).await.map(Some)
        } else {
            Ok(None)
        }
    }

    async fn maybe_finish_blob(
        &mut self,
        transfer: u64,
    ) -> std::result::Result<ServerMessage, WsFailure> {
        let complete = self
            .transfers
            .get(&transfer)
            .map(|state| state.next_offset == state.expected_len)
            .unwrap_or(false);

        if !complete {
            return Ok(ServerMessage::Ack {
                rev: self.current_rev().await?,
            });
        }

        let state = self
            .transfers
            .remove(&transfer)
            .ok_or_else(|| WsFailure::close(ErrorCode::Internal, "transfer inexistente"))?;

        let vault_id = self.vault_id.ok_or_else(|| {
            WsFailure::close(ErrorCode::Unauthorized, "vault no autenticado")
        })?;

        let digest = Sha256::digest(&state.bytes);
        let mut digest_bytes = [0u8; 32];
        digest_bytes.copy_from_slice(&digest);
        let calculated_blob_id = ovaultd_protocol::BlobId::new(digest_bytes);

        if calculated_blob_id != state.blob_id {
            return Err(WsFailure::error(
                ErrorCode::BadRequest,
                "el hash del blob no coincide con blob_id",
            ));
        }

        let size = i64::try_from(state.bytes.len())
            .map_err(|_| WsFailure::close(ErrorCode::BadRequest, "blob demasiado grande"))?;

        // Cuota opt-in (ADR-0012): el blob ya viajó por el socket, pero sin espacio no se
        // publica ni se registra.
        if !crate::storage::espacio_disponible(&self.pool, vault_id, &state.blob_id, size)
            .await
            .map_err(WsFailure::from_server_error)?
        {
            return Err(WsFailure::error(
                ErrorCode::QuotaExceeded,
                "la cuota del vault se agotaría con esta subida",
            ));
        }

        self.cas
            .publish(&state.blob_id, &state.bytes)
            .await
            .map_err(WsFailure::from_server_error)?;

        // El CAS no participa de la transacción del commit: primero el blob, después la referencia.
        // Sin esta fila, el batch que apunte a este blob recibiría `not_found` (`blob_exists`).
        crate::storage::registrar_blob(&self.pool, vault_id, &state.blob_id, size)
            .await
            .map_err(WsFailure::from_server_error)?;

        Ok(ServerMessage::Ack {
            rev: self.current_rev().await?,
        })
    }

    async fn pull(
        &mut self,
        pull: PullMessage,
    ) -> std::result::Result<ServerMessage, WsFailure> {
        self.require_authenticated()?;

        let head = self.current_rev().await?;
        self.since = pull.since.min(head);

        Ok(ServerMessage::PullAck {
            rev: self.since,
            ok: true,
        })
    }

    fn ping(&self) -> std::result::Result<ServerMessage, WsFailure> {
        self.require_authenticated()?;
        Ok(ServerMessage::Ping)
    }

    fn require_authenticated(&self) -> std::result::Result<(), WsFailure> {
        if self.vault_id.is_none() {
            return Err(WsFailure::close(
                ErrorCode::Unauthorized,
                "hello requerido",
            ));
        }

        if !self.authenticated {
            return Err(WsFailure::close(
                ErrorCode::Unauthorized,
                "auth requerido",
            ));
        }

        Ok(())
    }

    async fn current_rev(&self) -> std::result::Result<u64, WsFailure> {
        let vault_id = self.vault_id.ok_or_else(|| {
            WsFailure::close(ErrorCode::Unauthorized, "vault no autenticado")
        })?;

        let rev = sqlx::query_scalar::<_, i64>(
            "SELECT head_rev
             FROM vaults
             WHERE id = ?",
        )
        .bind(vault_id)
        .fetch_one(&self.pool)
        .await
        .map_err(ServerError::from)
        .map_err(WsFailure::from_server_error)?;

        u64::try_from(rev)
            .map_err(|_| WsFailure::close(ErrorCode::Internal, "revisión inválida"))
    }
}

struct WsFailure {
    message: ServerMessage,
    close: bool,
}

impl WsFailure {
    fn error(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            message: ServerMessage::Error {
                code: error_code_name(code).to_owned(),
                detail: detail.into(),
            },
            close: false,
        }
    }

    fn close(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            message: ServerMessage::Error {
                code: error_code_name(code).to_owned(),
                detail: detail.into(),
            },
            close: true,
        }
    }

    fn from_server_error(error: ServerError) -> Self {
        let code = error.error_code().unwrap_or(ErrorCode::Internal);
        Self::close(code, error.to_string())
    }
}

fn error_code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Unauthorized => "unauthorized",
        ErrorCode::NotFound => "not_found",
        ErrorCode::BadRequest => "bad_request",
        ErrorCode::MaxFileExceeded => "max_file_exceeded",
        ErrorCode::QuotaExceeded => "quota_exceeded",
        ErrorCode::ResyncRequired => "resync_required",
        ErrorCode::Internal => "internal",
    }
}

/// El `notify` de un registro recién commiteado, armado desde el oplog. El registry guarda
/// el blob ausente como hash cero (`delta::decode_blob_id`), así que acá se copia tal cual.
fn aviso_de_registro(registro: &SignedRecord, device: &str) -> Option<String> {
    let mensaje = ServerMessage::Notify {
        rev: registro.rev,
        op: registro.op,
        path_id: registro.path_id,
        blob_id: registro.blob_id,
        t_mac: registro.t_mac.clone(),
        sig: registro.sig.clone(),
        size: registro.size,
        device: device.to_string(),
        deleted: matches!(registro.op, Op::Del),
        prev_path_id: registro.prev_path_id,
    };

    serialize_message(&mensaje).ok()
}

fn serialize_message(message: &ServerMessage) -> Result<String> {
    serde_json::to_string(message)
        .map_err(|error| ServerError::Invalid(format!("no se pudo serializar WS: {error}")))
}

/// Único camino público para montar el WebSocket: exige autenticación para fijar la
/// identidad de la conexión. El token viene por `Authorization: Bearer` o — para
/// clientes de navegador, que no pueden fijar headers en `new WebSocket()` — por el
/// subprotocolo `ovaultd.bearer.<token en base64url sin padding>` (ADR-0015).
/// El server negocia `ovaultd.v1` como subprotocolo de aplicación (lo ecoa; el bearer
/// NUNCA se ecoa).
pub async fn authenticated_handler(
    State(state): State<EstadoServidor>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response> {
    let (account_id, device_id) = autenticar_upgrade(&state, &headers, &upgrade).await?;

    Ok(upgrade
        .protocols(["ovaultd.v1"])
        .on_upgrade(move |socket| {
            let mut connection = Connection::new(&state);
            connection.account_id = Some(account_id);
            connection.device_id = Some(device_id);
            websocket_with_connection(socket, connection)
        }))
}

/// Autentica el upgrade: header primero (el camino de siempre), subprotocolo después.
async fn autenticar_upgrade(
    state: &EstadoServidor,
    headers: &HeaderMap,
    upgrade: &WebSocketUpgrade,
) -> Result<(i64, i64)> {
    if let Some(value) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        if let Some(token) = value
            .strip_prefix("Bearer ")
            .filter(|token| !token.is_empty())
        {
            return autenticar_token(state, token).await;
        }
    }

    const PREFIJO: &str = "ovaultd.bearer.";
    for protocolo in upgrade.requested_protocols() {
        let Ok(texto) = protocolo.to_str() else {
            continue;
        };
        let Some(b64) = texto.strip_prefix(PREFIJO) else {
            continue;
        };
        let token = URL_SAFE_NO_PAD
            .decode(b64)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or(ServerError::Unauthorized)?;
        return autenticar_token(state, &token).await;
    }

    Err(ServerError::Unauthorized)
}

async fn websocket_with_connection(socket: WebSocket, mut connection: Connection) {
    // Dos cosas despiertan esta conexión: su propio socket, y la señal de que OTRA conexión del
    // mismo vault commiteó. El aviso se reenvía tal cual y el cliente que lo recibe hace un pull,
    // así que el aviso nunca es fuente de verdad (si se pierde, el pull lo recupera).
    //
    // El socket va partido en sink/stream para que el `select!` pueda esperar un mensaje del
    // cliente y escribir un aviso sin prestar dos veces el mismo socket.
    let (mut sink, mut stream) = socket.split();
    let mut avisos: Option<broadcast::Receiver<crate::storage::Aviso>> = None;

    loop {
        // El canal del vault se conoce recién con el `hello`, así que se toma cuando el Connection
        // ya lo tiene; hasta entonces esta conexión sólo escucha su propio socket.
        if avisos.is_none() {
            avisos = connection.tomar_avisos();
        }

        let message = if let Some(canal) = avisos.as_mut() {
            tokio::select! {
                message = tokio::time::timeout(INACTIVIDAD, stream.next()) => match message {
                    Ok(message) => message,
                    Err(_) => break,
                },
                aviso = canal.recv() => {
                    match aviso {
                        // El eco de mi propio batch no sirve: el pull que dispararía no traería
                        // nada nuevo (yo acabo de subirlo y el server ya me devolvió su rev).
                        Ok(aviso) if Some(aviso.de_device) == connection.device_id => {
                        }
                        Ok(aviso) => {
                            if sink
                                .send(Message::Text(aviso.texto.as_str().into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        // Receptor atrasado: se saltea avisos y el pull del cliente con su `since`
                        // recupera igual lo que quedó en el camino. No es un error.
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => avisos = None,
                    }
                    continue;
                }
            }
        } else {
            match tokio::time::timeout(INACTIVIDAD, stream.next()).await {
                Ok(message) => message,
                Err(_) => break,
            }
        };

        let Some(message) = message else { break };

        let message = match message {
            Ok(message) => message,
            Err(_) => break,
        };

        let handled = match message {
            Message::Text(text) => connection.handle_text(text.as_str()).await.map(Some),
            Message::Binary(frame) => connection.handle_binary(frame.as_ref()).await,
            Message::Ping(payload) => {
                if sink.send(Message::Pong(payload)).await.is_err() {
                    break;
                }
                Ok(None)
            }
            Message::Pong(_) => Ok(None),
            Message::Close(_) => break,
        };

        match handled {
            Ok(Some(response)) => {
                let Ok(text) = serialize_message(&response) else {
                    break;
                };

                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            Ok(None) => {}
            Err(failure) => {
                let Ok(text) = serialize_message(&failure.message) else {
                    break;
                };

                let _ = sink.send(Message::Text(text.into())).await;

                if failure.close {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    }
}

// HALLAZGO:
// - El contrato documenta `ovaultd_crypto::verify_proof(vault_pubkey, challenge,
//   vault_id, device_id, proof)`, mientras que la firma disponible en
//   `ovaultd-crypto` es `verify_proof(vault_pubkey, vault_id, device_id, challenge,
//   proof)`. Se usa la firma disponible.
// - El contrato pide que el handler del WS autentique el device antes del
//   handshake, pero `WebSocketUpgrade` no ejecuta automáticamente el extractor
//   `Autenticado`; por eso se expone `authenticated_handler`, que recibe el
//   extractor y conserva `account_id`/`device_id` por conexión.
// - El hash se calcula al completarse la transferencia, cuando todos los bytes
//   ya están disponibles en `BlobTransfer::bytes`; se compara contra el
//   `blob_id` declarado antes de publicar en el CAS.
// - El contrato exige que el blob inline sea asociado al vault, pero
//   `CasStore::publish` sólo recibe `(BlobId, bytes)` y no recibe `vault_id`.
//   Se delega tal cual en el CAS existente.

#[cfg(test)]
mod tests;

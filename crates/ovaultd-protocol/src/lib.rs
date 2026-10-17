#![forbid(unsafe_code)]

//! Tipos compartidos del protocolo `sync2`.
//!
//! Este crate no realiza I/O ni contiene lógica de transporte.

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// Techo duro de tamaño de contenido: 32 MiB (ADR-0007).
use base64::Engine as _;

/// Codificacion del wire: **base64** (`docs/protocol.md` usa `"<b64>"` para los identificadores y las firmas).
/// No confundir con `Display`/`FromStr`, que son **hex** y existen solo para logs y CLI.
pub(crate) fn b64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decodifica base64 del wire. Devuelve el error del motor si el texto no es base64 valido.
pub(crate) fn b64_decode(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    base64::engine::general_purpose::STANDARD.decode(text)
}

mod b64_bytes {
    use super::b64_decode;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&super::b64_encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        b64_decode(&text).map_err(serde::de::Error::custom)
    }
}

/// Tope de **contenido** (plaintext) por archivo: `docs/protocol.md` § "Cuotas y tamaño de contenido".
/// El cuerpo cifrado que viaja agrega el sobre `nonce[24] ‖ tag[16]` (ADR-0014): al comparar bytes
/// del wire, el tope es `MAX_FILE_BYTES + BLOB_ENVELOPE_BYTES`.
pub const MAX_FILE_BYTES: u64 = 33_554_432;

/// Tope de operaciones por batch (constante de protocolo -> `bad_request` si se supera).
pub const MAX_BATCH_OPS: usize = 1_000;

/// Versión de cifrado del protocolo.
pub const ENC_VERSION: &str = "sync2";

/// Operación aplicada sobre una entrada del vault.
///
/// Sale de `docs/protocol.md`, sección “Operaciones”, y de
/// `docs/implementation-map.md`, sección 1.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum Op {
    /// Agrega una entrada.
    Add = 1,
    /// Modifica una entrada.
    Mod = 2,
    /// Elimina una entrada mediante tombstone.
    Del = 3,
    /// Mueve o renombra una entrada.
    Move = 4,
}

impl TryFrom<u8> for Op {
    type Error = InvalidOp;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Add),
            2 => Ok(Self::Mod),
            3 => Ok(Self::Del),
            4 => Ok(Self::Move),
            _ => Err(InvalidOp(value)),
        }
    }
}

impl From<Op> for u8 {
    fn from(value: Op) -> Self {
        value as u8
    }
}

impl Serialize for Op {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            Self::Add => "add",
            Self::Mod => "mod",
            Self::Del => "del",
            Self::Move => "move",
        })
    }
}

impl<'de> Deserialize<'de> for Op {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OpVisitor;

        impl<'de> Visitor<'de> for OpVisitor {
            type Value = Op;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("add, mod, mod, del, move, or the numeric operation code")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                match value {
                    "add" => Ok(Op::Add),
                    "mod" => Ok(Op::Mod),
                    "del" => Ok(Op::Del),
                    "move" => Ok(Op::Move),
                    _ => Err(E::custom("invalid operation")),
                }
            }

            fn visit_u8<E>(self, value: u8) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Op::try_from(value).map_err(|_| E::custom("invalid operation"))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let value = u8::try_from(value).map_err(|_| E::custom("invalid operation"))?;
                self.visit_u8(value)
            }
        }

        deserializer.deserialize_any(OpVisitor)
    }
}

/// Error de conversión de un código de operación inválido.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidOp(pub u8);

impl fmt::Display for InvalidOp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid operation code {}", self.0)
    }
}

impl std::error::Error for InvalidOp {}

/// Códigos de error del protocolo (`docs/protocol.md` § Errores).
///
///
/// Lista **cerrada**: ampliarla requiere un ADR. El `code` **no** lleva prefijo `t:` — el prefijo `t:` es de los
/// **mensajes**, no de los errores. Un CAS fallido **no** es un error: devuelve el mensaje `t:conflict`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCode {
    /// Dispositivo revocado, firma de auth inválida o ACL ajena.
    Unauthorized,
    /// Vault, revisión o blob inexistente.
    NotFound,
    /// Mensaje mal formado o campo faltante.
    BadRequest,
    /// El contenido supera el tope duro de ADR-0007.
    MaxFileExceeded,
    /// La cuota opt-in del vault se agotaría con esta subida.
    QuotaExceeded,
    /// `since < min_rev`: hay que resincronizar completo, nunca una delta silenciosa.
    ResyncRequired,
    /// Fallo del servidor; el cliente reintenta con backoff.
    Internal,
}

impl ErrorCode {
    /// La lista **cerrada** del protocolo. Ampliarla requiere un ADR.
    pub const ALL: [Self; 7] = [
        Self::Unauthorized,
        Self::NotFound,
        Self::BadRequest,
        Self::MaxFileExceeded,
        Self::QuotaExceeded,
        Self::ResyncRequired,
        Self::Internal,
    ];

    /// El `code` exacto del wire. **Sin** prefijo `t:`: el prefijo es de los mensajes, no de los errores.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not_found",
            Self::BadRequest => "bad_request",
            Self::MaxFileExceeded => "max_file_exceeded",
            Self::QuotaExceeded => "quota_exceeded",
            Self::ResyncRequired => "resync_required",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ErrorCode {
    type Err = InvalidErrorCode;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|code| code.as_str() == value)
            .ok_or(InvalidErrorCode)
    }
}

impl Serialize for ErrorCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value
            .parse()
            .map_err(|_| de::Error::custom("invalid error code"))
    }
}

/// Error producido al convertir un código textual desconocido.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidErrorCode;

impl fmt::Display for InvalidErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid error code")
    }
}

impl std::error::Error for InvalidErrorCode {}

macro_rules! byte_id {
    ($name:ident, $size:expr, $section:literal) => {
        #[doc = $section]
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(pub [u8; $size]);

        impl $name {
            /// Construye el identificador desde sus bytes.
            pub const fn new(bytes: [u8; $size]) -> Self {
                Self(bytes)
            }

            /// Devuelve los bytes del identificador.
            pub const fn as_bytes(&self) -> &[u8; $size] {
                &self.0
            }
        }

        // Serde del wire: base64. `Display`/`FromStr` (hex) son para humanos, no para el protocolo.
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&b64_encode(&self.0))
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                let bytes = b64_decode(&text).map_err(serde::de::Error::custom)?;
                if bytes.len() != $size {
                    return Err(serde::de::Error::custom(concat!(
                        "longitud invalida para ",
                        stringify!($name)
                    )));
                }
                let mut out = [0u8; $size];
                out.copy_from_slice(&bytes);
                Ok(Self(out))
            }
        }

        impl From<[u8; $size]> for $name {
            fn from(value: [u8; $size]) -> Self {
                Self(value)
            }
        }

        impl From<$name> for [u8; $size] {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl From<$name> for Vec<u8> {
            fn from(value: $name) -> Self {
                value.0.to_vec()
            }
        }

        impl TryFrom<Vec<u8>> for $name {
            type Error = &'static str;

            fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
                if value.len() != $size {
                    return Err("invalid identifier length");
                }

                let mut bytes = [0u8; $size];
                bytes.copy_from_slice(&value);
                Ok(Self(bytes))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                for byte in self.0 {
                    write!(formatter, "{byte:02x}")?;
                }
                Ok(())
            }
        }

        impl FromStr for $name {
            type Err = &'static str;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                if value.len() != $size * 2 {
                    return Err("invalid hexadecimal identifier length");
                }

                let mut bytes = [0u8; $size];
                for (index, byte) in bytes.iter_mut().enumerate() {
                    let start = index * 2;
                    *byte = u8::from_str_radix(&value[start..start + 2], 16)
                        .map_err(|_| "invalid hexadecimal identifier")?;
                }

                Ok(Self(bytes))
            }
        }
    };
}

byte_id!(
    VaultId,
    16,
    "Identificador de vault de 16 bytes, generado por el cliente antes de crear el vault. Sale de `docs/implementation-map.md`, sección 2."
);

byte_id!(
    PathId,
    32,
    "Identificador HMAC de path de 32 bytes. Sale de `docs/protocol.md`, sección “Operaciones”, y de `docs/implementation-map.md`, sección 1."
);

byte_id!(
    BlobId,
    32,
    "Identificador SHA-256 del ciphertext de 32 bytes. Sale de `docs/protocol.md`, sección “Principios”, y de `docs/implementation-map.md`, sección 1."
);

byte_id!(
    OperationId,
    16,
    "Identificador binario de operación de 16 bytes para idempotencia. Sale de `docs/protocol.md`, sección “Principios”, y de `docs/implementation-map.md`, sección 6."
);

impl OperationId {
    /// Parsea la forma del WIRE de un `operation_id`: hexadecimal con guiones tolerados
    /// (el UUID que manda el cliente en el sobre del batch, y la misma forma con la que el
    /// commit lo persiste "tal cual llega").
    ///
    /// Es el parser ÚNICO de esa forma: la escritura (verificación de firma en el commit) y
    /// la lectura (delta) tienen que parsear igual. Tener dos — esto y un `base64::decode` —
    /// fue un bug real: la delta respondía 400 con datos que el propio commit había escrito.
    pub fn from_wire(texto: &str) -> Result<Self, &'static str> {
        let mut bytes = [0u8; 16];
        let mut index = 0usize;
        let mut nibble_alto = None;

        for byte in texto.bytes() {
            if byte == b'-' {
                continue;
            }

            let nibble = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return Err("operation_id con caracter inválido"),
            };

            if let Some(alto) = nibble_alto.take() {
                if index >= bytes.len() {
                    return Err("operation_id demasiado largo");
                }
                bytes[index] = (alto << 4) | nibble;
                index += 1;
            } else {
                nibble_alto = Some(nibble);
            }
        }

        if index == bytes.len() && nibble_alto.is_none() {
            Ok(Self(bytes))
        } else {
            Err("operation_id incompleto")
        }
    }
}

/// Revisión monotónica de un vault.
///
/// Sale de `docs/protocol.md`, sección “Principios”.
pub type Revision = u64;

/// Modos de sincronización y resolución de conflictos del handshake.
///
/// Sale de `docs/protocol.md`, sección “Handshake”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Modes {
    /// Modo de sincronización.
    pub sync: String,
    /// Estrategia de conflicto.
    pub conflict: String,
}

/// Mensaje `hello` del handshake.
///
/// Sale de `docs/protocol.md`, sección “Handshake”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    /// Tipo de mensaje.
    pub t: String,
    /// Versión negociada del protocolo de aplicación.
    pub proto: u64,
    /// Vault objetivo.
    pub vault: u64,
    /// Dispositivo que inicia el handshake.
    pub device: u64,
    /// Última revisión aplicada por el cliente.
    pub since: Revision,
    /// Modos elegidos por el cliente.
    pub modes: Modes,
}

/// Respuesta `hello_ok` del handshake.
///
/// Sale de `docs/protocol.md`, sección “Handshake”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HelloOk {
    /// Tipo de mensaje.
    pub t: String,
    /// Challenge efímero codificado en base64.
    pub challenge: String,
    /// Última revisión del vault.
    pub head_rev: Revision,
}

/// Mensaje `auth` del handshake.
///
/// Sale de `docs/protocol.md`, sección “Handshake”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Auth {
    /// Tipo de mensaje.
    pub t: String,
    /// Firma del challenge, vault y device, codificada en base64.
    pub proof: String,
}

/// Respuesta `auth_ok` del handshake.
///
/// Sale de `docs/protocol.md`, sección “Handshake”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthOk {
    /// Tipo de mensaje.
    pub t: String,
    /// Revisión conocida por el servidor al autenticar.
    pub rev: Revision,
}

/// Solicitud de creación de vault.
///
/// Sale de `docs/protocol.md`, sección “Creación del vault”, y de
/// `docs/implementation-map.md`, sección 2.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateVault {
    /// Nombre del vault.
    pub name: String,
    /// Versión de cifrado.
    pub enc_version: String,
    /// Identificador elegido por el cliente.
    pub vault_id: VaultId,
    /// Salt de derivación.
    #[serde(with = "b64_bytes")]
    pub salt: Vec<u8>,
    /// Algoritmo KDF.
    pub kdf_alg: String,
    /// Parámetros del KDF.
    pub kdf_params: String,
    /// Clave envuelta con la contraseña.
    #[serde(with = "b64_bytes")]
    pub wrapped_pw: Vec<u8>,
    /// Clave envuelta con la clave de recuperación.
    #[serde(with = "b64_bytes")]
    pub wrapped_rc: Vec<u8>,
    /// Clave pública del vault.
    #[serde(with = "b64_bytes")]
    pub vault_pubkey: Vec<u8>,
}

/// Respuesta de creación de vault.
///
/// Sale de `docs/protocol.md`, sección “Control”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateVaultResponse {
    /// Identificador del vault creado.
    pub id: VaultId,
}

/// Una operación dentro de un batch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchOp {
    /// Operación aplicada sobre una entrada del vault.
    pub op: Op,
    /// Identificador del path destino.
    pub path_id: PathId,
    /// Nombre cifrado en base64; `None` solo en `del`.
    pub path_enc: Option<String>,
    /// Indica explícitamente si la entrada es una carpeta.
    pub folder: bool,
    /// Identificador del blob.
    pub blob_id: Option<BlobId>,
    /// MAC del binding path/blob, codificado en base64.
    pub t_mac: Option<String>,
    /// Firma del registro, codificada en base64.
    pub sig: String,
    /// Tamaño del contenido.
    pub size: Option<u64>,
    /// Fecha de modificación en milisegundos.
    pub mtime: Option<u64>,
    /// Fecha de creación en milisegundos.
    pub ctime: Option<u64>,
    /// Path anterior para operaciones move.
    pub prev_path_id: Option<PathId>,
}

impl BatchOp {
    /// Valida las restricciones de una operación dentro del batch.
    pub fn validate(&self) -> Result<(), ErrorCode> {
        match self.op {
            Op::Del => {
                if self.blob_id.is_some() || self.path_enc.is_some() {
                    return Err(ErrorCode::BadRequest);
                }
            }
            Op::Add | Op::Mod => {
                if self.path_enc.is_none() {
                    return Err(ErrorCode::BadRequest);
                }

                if self.folder {
                    if self.blob_id.is_some() || self.size.is_some() {
                        return Err(ErrorCode::BadRequest);
                    }
                } else {
                    if self.blob_id.is_none() || self.size.is_none() {
                        return Err(ErrorCode::BadRequest);
                    }

                    check_content_size(self.size.expect("size was checked as present"))?;
                }
            }
            Op::Move => {
                if self.path_enc.is_none() || self.prev_path_id.is_none() {
                    return Err(ErrorCode::BadRequest);
                }

                if self.folder {
                    if self.blob_id.is_some() || self.size.is_some() {
                        return Err(ErrorCode::BadRequest);
                    }
                } else {
                    if self.blob_id.is_none() || self.size.is_none() {
                        return Err(ErrorCode::BadRequest);
                    }

                    check_content_size(self.size.expect("size was checked as present"))?;
                }
            }
        }

        Ok(())
    }
}

/// El sobre: N operaciones, UN expected_rev, UNA revision.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Batch {
    /// Tipo de mensaje.
    ///
    /// Opcional: por HTTP el cliente lo manda, pero por WebSocket el enum `ClientMessage` **consume `t` como su
    /// etiqueta** (`#[serde(tag = "t")]`) y al aplanar (`#[serde(flatten)]`) este struct ya no la encuentra. Sin
    /// `default`, el batch por WS no se puede deserializar: "missing field `t`".
    #[serde(default)]
    pub t: String,
    /// Identificador del batch: string UUID que genera el cliente. El server lo guarda y lo
    /// devuelve **tal cual llega** (nunca lo interpreta).
    pub batch_id: String,
    /// Identificador idempotente de la operación: UUID hex de 16 bytes, **guiones tolerados**
    /// (`OperationId::from_wire`). NO base64.
    pub operation_id: String,
    /// Revisión contra la que se hace compare-and-swap.
    pub expected_rev: Revision,
    /// Operaciones del batch.
    pub ops: Vec<BatchOp>,
}

impl Batch {
    /// Valida las restricciones del batch definidas por el protocolo.
    pub fn validate(&self) -> Result<(), ErrorCode> {
        if self.ops.is_empty() {
            return Err(ErrorCode::BadRequest);
        }

        if self.ops.len() > MAX_BATCH_OPS {
            return Err(ErrorCode::BadRequest);
        }

        for op in &self.ops {
            op.validate()?;

            if op.sig.is_empty() {
                return Err(ErrorCode::BadRequest);
            }
        }

        Ok(())
    }
}

/// Acuse del batch: UNA revision para todo el batch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchAck {
    /// Tipo de mensaje.
    pub t: String,
    /// Identificador del batch.
    pub batch_id: String,
    /// Revisión creada para todo el batch.
    pub rev: Revision,
}

/// Mensaje `pull_ack`.
///
/// Sale de `docs/protocol.md`, sección “Operaciones”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PullAck {
    /// Tipo de mensaje.
    pub t: String,
    /// Revisión confirmada.
    pub rev: Revision,
    /// Resultado de la operación.
    pub ok: bool,
}

/// Mensaje `ping`.
///
/// Sale de `docs/protocol.md`, sección “Operaciones”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ping {
    /// Tipo de mensaje.
    pub t: String,
}

/// Registro enviado en una notificación `notify`.
///
/// Sale de `docs/protocol.md`, sección “Operaciones”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Notify {
    /// Tipo de mensaje.
    pub t: String,
    /// Revisión del registro.
    pub rev: Revision,
    /// Operación.
    pub op: Op,
    /// Identificador del path.
    pub path_id: PathId,
    /// Identificador del blob.
    pub blob_id: BlobId,
    /// MAC del binding path/blob.
    pub t_mac: String,
    /// Firma del registro.
    pub sig: String,
    /// Tamaño del contenido.
    pub size: u64,
    /// Dispositivo que produjo el registro.
    pub device: String,
    /// Indica un tombstone.
    pub deleted: bool,
    /// Identificador del path anterior.
    pub prev_path_id: Option<PathId>,
}

/// Registro persistido y firmado de una revisión.
///
/// Sale de `docs/protocol.md`, sección “Registros firmados”, y de
/// `docs/implementation-map.md`, secciones 1, 3, 4 y 6.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedRecord {
    /// Revisión del registro.
    pub rev: Revision,
    /// Operación.
    pub op: Op,
    /// Identificador del path.
    pub path_id: PathId,
    /// Identificador del blob.
    pub blob_id: BlobId,
    /// Path cifrado, cuando se obtiene historia.
    #[serde(with = "b64_bytes")]
    pub path_enc: Vec<u8>,
    /// MAC del binding path/blob.
    pub t_mac: String,
    /// Firma del registro.
    pub sig: String,
    /// Tamaño del contenido.
    pub size: u64,
    /// Fecha de modificación en milisegundos.
    pub mtime: u64,
    /// Revisión base esperada.
    pub expected_rev: Revision,
    /// Identificador idempotente de operación.
    pub operation_id: OperationId,
    /// Epoch de la clave del vault.
    pub epoch: u16,
    /// Identificador del path anterior.
    pub prev_path_id: Option<PathId>,
}

/// Cabecera de transferencia inline de blob.
///
/// Sale de `docs/protocol.md`, sección “Transporte binario”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlobHead {
    /// Tipo de mensaje.
    pub t: String,
    /// Revisión que publica el blob.
    pub rev: Revision,
    /// Identificador de transferencia.
    pub transfer: String,
    /// Longitud total del blob.
    pub len: u64,
    /// Offset inicial.
    pub offset: u64,
}

/// Confirmación de revisión.
///
/// Sale de `docs/protocol.md`, sección “Operaciones”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ack {
    /// Tipo de mensaje.
    pub t: String,
    /// Revisión confirmada.
    pub rev: Revision,
}

/// Conflicto de compare-and-swap.
///
/// Sale de `docs/protocol.md`, sección “Operaciones”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conflict {
    /// Tipo de mensaje.
    pub t: String,
    /// Identificador del path en conflicto.
    pub path_id: PathId,
    /// Revisión base del cliente.
    pub base_rev: Revision,
    /// Revisión actual del servidor.
    pub server_rev: Revision,
    /// Revisión del cliente.
    pub client_rev: Revision,
}

/// Error de protocolo.
///
/// Sale de `docs/protocol.md`, sección “Operaciones”.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProtocolError {
    /// Tipo de mensaje.
    pub t: String,
    /// Código exacto del error.
    pub code: ErrorCode,
    /// Detalle legible del error.
    pub detail: String,
}

/// Página de cambios.
///
/// Sale de `docs/protocol.md`, sección “Control”, y de
/// `docs/implementation-map.md`, sección 5.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Changes {
    /// Registros completos, sin cortar una revisión.
    pub items: Vec<SignedRecord>,
    /// Cursor de la última revisión completa incluida.
    pub next_since: Revision,
    /// Piso de retención, si el cliente quedó fuera de ventana.
    pub min_rev: Option<Revision>,
}

/// Valida el tamaño de contenido contra el techo duro del protocolo.
///
/// Sale de `docs/protocol.md`, sección “Cuotas y tamaño de contenido”.
pub fn check_content_size(size: u64) -> Result<(), ErrorCode> {
    if size > MAX_FILE_BYTES {
        Err(ErrorCode::MaxFileExceeded)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn los_campos_binarios_viajan_en_base64() {
        let salt = vec![1, 2, 3, 4];
        let wrapped_pw = vec![5, 6, 7, 8];
        let wrapped_rc = vec![9, 10, 11, 12];
        let vault_pubkey = vec![13, 14, 15, 16];
        let path_enc = vec![17, 18, 19, 20];

        let vault = CreateVault {
            name: "vault".to_owned(),
            enc_version: ENC_VERSION.to_owned(),
            vault_id: VaultId::new([21; 16]),
            salt: salt.clone(),
            kdf_alg: "argon2id".to_owned(),
            kdf_params: "params".to_owned(),
            wrapped_pw: wrapped_pw.clone(),
            wrapped_rc: wrapped_rc.clone(),
            vault_pubkey: vault_pubkey.clone(),
        };

        let vault_json = serde_json::to_value(vault).expect("vault should serialize");

        for (field, expected) in [
            ("salt", salt),
            ("wrapped_pw", wrapped_pw),
            ("wrapped_rc", wrapped_rc),
            ("vault_pubkey", vault_pubkey),
        ] {
            let value = &vault_json[field];
            let Value::String(encoded) = value else {
                panic!("{field} should be serialized as a JSON string");
            };
            assert_eq!(b64_decode(encoded).expect("base64 should decode"), expected);
        }

        let record = SignedRecord {
            rev: 1,
            op: Op::Mod,
            path_id: PathId::new([22; 32]),
            blob_id: BlobId::new([23; 32]),
            path_enc: path_enc.clone(),
            t_mac: "mac".to_owned(),
            sig: "signature".to_owned(),
            size: 20,
            mtime: 21,
            expected_rev: 0,
            operation_id: OperationId::new([24; 16]),
            epoch: 1,
            prev_path_id: None,
        };

        let record_json = serde_json::to_value(record).expect("record should serialize");
        let Value::String(encoded) = &record_json["path_enc"] else {
            panic!("path_enc should be serialized as a JSON string");
        };
        assert_eq!(
            b64_decode(encoded).expect("base64 should decode"),
            path_enc
        );
    }

    #[test]
    fn base64_invalido_es_rechazado() {
        let value = json!({
            "name": "vault",
            "enc_version": ENC_VERSION,
            "vault_id": b64_encode(&[0; 16]),
            "salt": "no es base64!!",
            "kdf_alg": "argon2id",
            "kdf_params": "params",
            "wrapped_pw": b64_encode(&[1, 2, 3]),
            "wrapped_rc": b64_encode(&[4, 5, 6]),
            "vault_pubkey": b64_encode(&[7, 8, 9])
        });

        assert!(serde_json::from_value::<CreateVault>(value).is_err());
    }

    #[test]
    fn operation_id_from_wire_acepta_uuid_y_hex_pelado() {
        let uuid = "9493f4fc-84e2-4466-a4e3-02942abffde1";

        let esperado = OperationId::new([
            0x94, 0x93, 0xf4, 0xfc, 0x84, 0xe2, 0x44, 0x66, 0xa4, 0xe3, 0x02, 0x94, 0x2a, 0xbf,
            0xfd, 0xe1,
        ]);

        assert_eq!(OperationId::from_wire(uuid).expect("uuid con guiones"), esperado);
        assert_eq!(
            OperationId::from_wire("9493f4fc84e24466a4e302942abffde1").expect("hex pelado"),
            esperado
        );
    }

    #[test]
    fn operation_id_from_wire_rechaza_lo_que_no_es_16_bytes_hex() {
        // Corto (15 bytes).
        assert!(OperationId::from_wire("9493f4fc-84e2-4466-a4e3-02942abffde").is_err());
        // Largo (un hex de más).
        assert!(OperationId::from_wire("9493f4fc-84e2-4466-a4e3-02942abffde1f").is_err());
        // No hex.
        assert!(OperationId::from_wire("no-es-un-operation-id-en-hexadecimal").is_err());
    }
}

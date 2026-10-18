use axum::{
    http::{header::HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use ovaultd_protocol::{
    Conflict as ProtocolConflict, ErrorCode, PathId, ProtocolError,
};

pub type Result<T> = std::result::Result<T, ServerError>;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("no autorizado")]
    Unauthorized,

    #[error("prohibido")]
    Forbidden,

    #[error("no encontrado")]
    NotFound,

    #[error("conflicto en path {path_id}: rev {expected}, head {head}")]
    Conflict {
        path_id: PathId,
        expected: u64,
        head: u64,
    },

    #[error("código de protocolo: {0}")]
    Protocol(ErrorCode),

    #[error("entrada inválida: {0}")]
    Invalid(String),

    /// Freno de login (429): demasiados intentos en la ventana. El `Retry-After` viaja en
    /// el `IntoResponse`; acá no lleva datos (el detalle es fijo).
    #[error("demasiados intentos")]
    Frenado,

    /// El offset de un PATCH no sigue al confirmado (hueco o repetición): 409 con el offset
    /// actual en el header `Upload-Offset` para que el cliente reanude sin adivinar (ADR-0012).
    #[error("offset no sigue: actual {actual}, pedido {requested}")]
    UploadOffset { actual: i64, requested: i64 },

    #[error(transparent)]
    Db(#[from] sqlx::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ServerError {
    pub fn error_code(&self) -> Option<ErrorCode> {
        match self {
            Self::Unauthorized | Self::Forbidden => Some(ErrorCode::Unauthorized),
            Self::NotFound => Some(ErrorCode::NotFound),
            Self::Conflict { .. } => None,
            Self::Protocol(code) => Some(*code),
            Self::Invalid(_) => Some(ErrorCode::BadRequest),
            Self::Frenado => Some(ErrorCode::Unauthorized),
            Self::UploadOffset { .. } => Some(ErrorCode::BadRequest),
            Self::Db(_) | Self::Io(_) => Some(ErrorCode::Internal),
        }
    }

    fn status_code(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict { .. } => StatusCode::CONFLICT,
            Self::Protocol(code) => status_for_error_code(*code),
            Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::Frenado => StatusCode::TOO_MANY_REQUESTS,
            Self::UploadOffset { .. } => StatusCode::CONFLICT,
            Self::Db(_) | Self::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        match self {
            Self::Conflict {
                path_id,
                expected,
                head,
            } => (
                StatusCode::CONFLICT,
                Json(ProtocolConflict {
                    t: "conflict".to_owned(),
                    path_id,
                    base_rev: expected,
                    server_rev: head,
                    client_rev: expected,
                }),
            )
                .into_response(),

            Self::Db(error) => {
                tracing::error!(error = %error, "database error");
                protocol_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorCode::Internal,
                    "error interno del servidor".to_owned(),
                )
            }

            Self::Io(error) => {
                tracing::error!(error = %error, "I/O error");
                protocol_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorCode::Internal,
                    "error interno del servidor".to_owned(),
                )
            }

            // 409 + `Upload-Offset` actual: el cliente reanuda sin adivinar dónde quedó (ADR-0012).
            Self::UploadOffset { actual, requested } => (
                StatusCode::CONFLICT,
                [(
                    HeaderName::from_static("upload-offset"),
                    HeaderValue::from_str(&actual.to_string())
                        .unwrap_or_else(|_| HeaderValue::from_static("0")),
                )],
                Json(ProtocolError {
                    t: "error".to_owned(),
                    code: ErrorCode::BadRequest,
                    detail: format!("offset no sigue: actual {actual}, pedido {requested}"),
                }),
            )
                .into_response(),

            // Freno de login (429): `Retry-After` + el formato de error del protocolo.
            Self::Frenado => (
                StatusCode::TOO_MANY_REQUESTS,
                [(axum::http::header::RETRY_AFTER, "60")],
                Json(ProtocolError {
                    t: "error".to_owned(),
                    code: ErrorCode::Unauthorized,
                    detail: "demasiados intentos: esperá un minuto".to_owned(),
                }),
            )
                .into_response(),

            error => {
                let status = error.status_code();
                let code = error
                    .error_code()
                    .unwrap_or(ErrorCode::Internal);

                let detail = match error {
                    Self::Unauthorized => "no autorizado".to_owned(),
                    Self::Forbidden => "prohibido".to_owned(),
                    Self::NotFound => "no encontrado".to_owned(),
                    Self::Protocol(_) => error.to_string(),
                    Self::Invalid(detail) => detail,
                    Self::Conflict { .. }
                    | Self::UploadOffset { .. }
                    | Self::Frenado
                    | Self::Db(_)
                    | Self::Io(_) => "error interno del servidor".to_owned(),
                };

                protocol_response(status, code, detail)
            }
        }
    }
}

fn status_for_error_code(code: ErrorCode) -> StatusCode {
    match code {
        ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
        ErrorCode::MaxFileExceeded => StatusCode::PAYLOAD_TOO_LARGE,
        ErrorCode::QuotaExceeded => StatusCode::PAYLOAD_TOO_LARGE,
        ErrorCode::ResyncRequired => StatusCode::GONE,
        ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn protocol_response(status: StatusCode, code: ErrorCode, detail: String) -> Response {
    (
        status,
        Json(ProtocolError {
            t: "error".to_owned(),
            code,
            detail,
        }),
    )
        .into_response()
}

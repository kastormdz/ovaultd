//! Contexto compartido de los tests de integración.
//!
//! Estos tests usan SOLO la API pública del crate, como un consumidor externo: base SQLite real
//! en un tempdir (migrada), CAS real, y el router completo para la parte HTTP. Las firmas se
//! arman con `ovaultd-crypto`, igual que las armaría un cliente.
#![allow(dead_code)]

use std::sync::Arc;

use axum::{
    body::Body,
    http::{header::AUTHORIZATION, HeaderValue, Method, Request},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use http_body_util::BodyExt;
use ovaultd_crypto::{derive_subkeys, sign_record, SubKeys, VaultIdentity, VaultKey};
use ovaultd_protocol::{Batch, BatchOp, BlobId, Op, PathId};
use ovaultd_server::{
    auth,
    config::Config,
    db,
    routes::{self, EstadoServidor},
    storage::CasStore,
};
use serde_json::Value;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

pub const EMAIL: &str = "integracion@example.com";
pub const PASSWORD: &str = "password-de-integracion-del-vault";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct TestContext {
    pub pool: SqlitePool,
    _db_dir: TempDir,
    _cas_dir: TempDir,
    pub config: Config,
    pub cas: Arc<CasStore>,
    /// Compartido entre requests (el `estado()` clona el Arc): el freno de intentos del
    /// panel acumula como en producción (ADR-0017).
    pub limitador_login: Arc<ovaultd_server::admin::sesiones::LimitadorDeLogin>,
    /// Compartido entre requests igual que el freno del login: el `estado()` clona el Arc, así que
    /// un aviso emitido desde un request llega a la conexión WebSocket del test.
    pub notifier: Arc<ovaultd_server::storage::Notifier>,
    pub account_id: i64,
    pub device_id: i64,
    pub token: String,
    pub vault_id: i64,
    pub epoch: u16,
    pub keys: SubKeys,
}

impl TestContext {
    pub async fn new() -> Result<Self> {
        let db_dir = tempfile::tempdir()?;
        let cas_dir = tempfile::tempdir()?;
        let db_path = db_dir.path().join("metadata.db");

        let pool = db::connect(db_path.to_str().ok_or("ruta inválida")?).await?;
        db::migrate(&pool).await?;

        let account = auth::crear_cuenta(&pool, EMAIL, PASSWORD).await?;
        let (token, device_id) = auth::iniciar_sesion(
            &pool,
            EMAIL,
            PASSWORD,
            "test-device",
            Some("integracion"),
        )
        .await?;

        let vault_key = VaultKey([7u8; 32]);
        let keys = derive_subkeys(&vault_key, &[8u8; 16]);
        let identity = VaultIdentity::generate(&keys.sign);

        let vault_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO vaults
                 (account_id, name, salt, kdf_params, wrapped_key_pw,
                  wrapped_key_rc, vault_pubkey)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(account.id)
        .bind("vault-de-integracion")
        .bind(vec![8u8; 16])
        .bind("{}")
        .bind(vec![0u8; 32])
        .bind(vec![0u8; 32])
        .bind(identity.pubkey().to_vec())
        .fetch_one(&pool)
        .await?;

        let epoch: i64 = sqlx::query_scalar("SELECT epoch FROM vaults WHERE id = ?")
            .bind(vault_id)
            .fetch_one(&pool)
            .await?;

        let config = Config {
            bind_addr: "127.0.0.1:0".to_owned(),
            database_path: db_path.to_string_lossy().into_owned(),
            cas_root: cas_dir.path().to_path_buf(),
            token_ttl_hours: 720,
            challenge_ttl_seconds: 300,
            upload_ttl_hours: 24,
            setup_token: None,
            secure_cookies: ovaultd_server::config::ModoSecureCookies::Auto,
        };
        let cas = Arc::new(CasStore::new(cas_dir.path().to_path_buf())?);

        Ok(Self {
            pool,
            _db_dir: db_dir,
            _cas_dir: cas_dir,
            config,
            cas,
            limitador_login: Default::default(),
            notifier: Default::default(),
            account_id: account.id,
            device_id,
            token,
            vault_id,
            epoch: u16::try_from(epoch)?,
            keys,
        })
    }

    /// Otro vault de la MISMA cuenta (aislamiento sin cambiar de identidad).
    pub async fn agregar_vault_propio(&self, nombre: &str) -> Result<i64> {
        let vault_key = VaultKey([11u8; 32]);
        let keys = derive_subkeys(&vault_key, &[12u8; 16]);
        let identity = VaultIdentity::generate(&keys.sign);

        Ok(sqlx::query_scalar::<_, i64>(
            "INSERT INTO vaults
                 (account_id, name, salt, kdf_params, wrapped_key_pw,
                  wrapped_key_rc, vault_pubkey)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(self.account_id)
        .bind(nombre)
        .bind(vec![12u8; 16])
        .bind("{}")
        .bind(vec![0u8; 32])
        .bind(vec![0u8; 32])
        .bind(identity.pubkey().to_vec())
        .fetch_one(&self.pool)
        .await?)
    }

    /// Otra cuenta con su propio vault y token (para el aislamiento cross-vault real).
    pub async fn agregar_vault_ajeno(&self, email: &str) -> Result<(i64, String)> {
        let cuenta = auth::crear_cuenta(&self.pool, email, PASSWORD).await?;
        let (token, _device) = auth::iniciar_sesion(
            &self.pool,
            email,
            PASSWORD,
            "device-ajeno",
            Some("integracion"),
        )
        .await?;

        let vault_key = VaultKey([9u8; 32]);
        let keys = derive_subkeys(&vault_key, &[10u8; 16]);
        let identity = VaultIdentity::generate(&keys.sign);

        let vault_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO vaults
                 (account_id, name, salt, kdf_params, wrapped_key_pw,
                  wrapped_key_rc, vault_pubkey)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(cuenta.id)
        .bind("vault-ajeno")
        .bind(vec![10u8; 16])
        .bind("{}")
        .bind(vec![0u8; 32])
        .bind(vec![0u8; 32])
        .bind(identity.pubkey().to_vec())
        .fetch_one(&self.pool)
        .await?;

        Ok((vault_id, token))
    }

    pub fn estado(&self) -> EstadoServidor {
        EstadoServidor {
            pool: self.pool.clone(),
            config: self.config.clone(),
            cas: self.cas.clone(),
            limitador_login: self.limitador_login.clone(),
            notifier: self.notifier.clone(),
        }
    }

    pub async fn request(&self, request: Request<Body>) -> Result<axum::response::Response> {
        Ok(routes::router(self.estado()).oneshot(request).await?)
    }

    pub async fn authenticated_request(
        &self,
        method: Method,
        uri: &str,
        body: Body,
    ) -> Result<axum::response::Response> {
        self.request_con_token(&self.token, method, uri, body).await
    }

    pub async fn request_con_token(
        &self,
        token: &str,
        method: Method,
        uri: &str,
        body: Body,
    ) -> Result<axum::response::Response> {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            )
            .body(body)?;

        self.request(request).await
    }

    pub fn identidad(&self) -> VaultIdentity {
        VaultIdentity::generate(&self.keys.sign)
    }

    /// Registra el blob para ESTE vault (lo que haría una subida real).
    pub async fn registrar_blob(&self, blob: u8, size: i64) -> Result<()> {
        self.registrar_blob_en(self.vault_id, blob, size).await
    }

    pub async fn registrar_blob_en(&self, vault_id: i64, blob: u8, size: i64) -> Result<()> {
        ovaultd_server::storage::registrar_blob(
            &self.pool,
            vault_id,
            &BlobId::new([blob; 32]),
            size,
        )
        .await?;
        Ok(())
    }

    /// Arma y firma un batch como lo haría un cliente (una sola fuente: se firma el op serializado).
    pub fn batch_firmado(&self, mut ops: Vec<BatchOp>, expected_rev: u64) -> Batch {
        let operation_id = Uuid::new_v4();
        let identity = self.identidad();

        for op in &mut ops {
            firmar_op(
                op,
                &identity,
                expected_rev,
                operation_id.as_bytes(),
                self.epoch,
            );
        }

        Batch {
            t: "batch".to_owned(),
            batch_id: Uuid::new_v4().to_string(),
            operation_id: operation_id.to_string(),
            expected_rev,
            ops,
        }
    }

    /// Commit contra la base real (el mismo `commit_batch` que usa la superficie HTTP/WS).
    pub async fn commit(
        &self,
        ops: Vec<BatchOp>,
        expected_rev: u64,
    ) -> ovaultd_server::Result<ovaultd_server::sync::commit::CommitOutcome> {
        let batch = self.batch_firmado(ops, expected_rev);
        ovaultd_server::sync::commit::commit_batch(
            &self.pool,
            self.vault_id,
            self.device_id,
            &batch,
        )
        .await
    }
}

/// El código de operación del WIRE (mapeo de `commit.rs::verify_batch_operation`).
pub fn op_code(op: Op) -> u8 {
    match op {
        Op::Add => 0,
        Op::Mod => 1,
        Op::Del => 2,
        Op::Move => 3,
    }
}

pub fn firmar_op(
    op: &mut BatchOp,
    identity: &VaultIdentity,
    expected_rev: u64,
    operation_id: &[u8; 16],
    epoch: u16,
) {
    let blob_id = op
        .blob_id
        .map(|valor| *valor.as_bytes())
        .unwrap_or([0u8; 32]);
    let t_mac = op
        .t_mac
        .as_deref()
        .map(|t| STANDARD.decode(t).expect("t_mac del helper es b64"))
        .map(|bytes| <[u8; 64]>::try_from(bytes).expect("t_mac del helper son 64 bytes"))
        .unwrap_or([0u8; 64]);

    let firma = sign_record(
        identity,
        op.path_id.as_bytes(),
        &blob_id,
        &t_mac,
        op_code(op.op),
        op.size.unwrap_or(0),
        op.mtime.unwrap_or(0),
        expected_rev,
        operation_id,
        epoch,
    );

    op.sig = STANDARD.encode(firma);
}

pub fn add_op(path: u8, blob: u8, size: u64) -> BatchOp {
    add_op_con_blob(path, BlobId::new([blob; 32]), size)
}

pub fn add_op_con_blob(path: u8, blob_id: BlobId, size: u64) -> BatchOp {
    BatchOp {
        op: Op::Add,
        path_id: PathId::new([path; 32]),
        path_enc: Some(STANDARD.encode([path; 8])),
        folder: false,
        blob_id: Some(blob_id),
        t_mac: Some(STANDARD.encode([0u8; 64])),
        sig: String::new(),
        size: Some(size),
        mtime: Some(1_758_831_000_000),
        ctime: Some(1_758_831_000_000),
        prev_path_id: None,
    }
}

pub fn del_op(path: u8) -> BatchOp {
    BatchOp {
        op: Op::Del,
        path_id: PathId::new([path; 32]),
        path_enc: None,
        folder: false,
        blob_id: None,
        t_mac: None,
        sig: String::new(),
        size: None,
        mtime: None,
        ctime: None,
        prev_path_id: None,
    }
}

pub fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub async fn response_bytes(response: axum::response::Response) -> Result<Vec<u8>> {
    let bytes = response.into_body().collect().await?.to_bytes();
    Ok(bytes.to_vec())
}

pub async fn response_json(response: axum::response::Response) -> Result<Value> {
    let bytes = response.into_body().collect().await?.to_bytes();
    Ok(serde_json::from_slice(&bytes)?)
}

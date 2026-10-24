use crate::error::{Result, ServerError};
use argon2::password_hash::phc::PasswordHash;
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool};
use std::sync::OnceLock;
use uuid::Uuid;

pub struct Cuenta {
    pub id: i64,
    pub email: String,
}

#[derive(Debug, FromRow)]
struct FilaCuenta {
    id: i64,
    email: String,
}

#[derive(Debug, FromRow)]
struct FilaCuentaLogin {
    id: i64,
    password_hash: String,
    disabled_at: Option<String>,
}

#[derive(Debug, FromRow)]
struct FilaToken {
    device_id: i64,
}

fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(default)
}

pub(crate) fn token_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(Uuid::new_v4().as_bytes());
    bytes
}

pub(crate) fn token_string(bytes: &[u8]) -> String {
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(token, "{byte:02x}");
    }
    token
}

pub(crate) fn hash_bytes(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

/// Comparación de tokens en TIEMPO CONSTANTE (sin early-return): la usan el setup_token
/// (ADR-0016) y el CSRF del panel (ADR-0017).
pub(crate) fn tokens_iguales(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }

    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn hash_token(token: &str) -> Vec<u8> {
    hash_bytes(token.as_bytes())
}

fn normalize_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database_error) => {
            database_error.message().contains("UNIQUE")
                || database_error.message().contains("unique")
        }
        _ => false,
    }
}

/// Mínimo de la password de CUENTA. Solo aplica a altas y cambios: las passwords
/// existentes no se invalidan (el verify lee los parámetros del propio PHC).
pub const MIN_PASSWORD: usize = 12;

/// Las más usadas del mundo (y las locales obvias), en minúsculas. Lista corta y local:
/// el chequeo contra HIBP exige una dependencia HTTP saliente y el server tiene que poder
/// correr air-gapped. Con el mínimo de 12 esto filtra lo que un atacante prueba primero.
const PASSWORDS_COMUNES: &[&str] = &[
    "123456789012", "1234567890123", "password1234", "password123!", "qwertyuiop12",
    "qwerty123456", "administrador", "contraseña123", "contrasena123", "admin1234567",
    "iloveyou1234", "micontrasena", "mipassword12", "argentina2024", "argentina2025",
    "cambiar12345", "temporal1234", "bienvenido12", "welcome12345", "letmein12345",
    "superman1234", "batman123456", "futbol123456", "riverplate12", "bocajuniors",
    "monotributo1", "computadora1", "windows12345", "android12345", "samsung12345",
    "correo123456", "usuario12345", "privado12345", "secreto12345", "seguridad123",
];

/// `true` si la password no cumple la política mínima (largo o lista de comunes).
pub fn password_debil(password: &str) -> bool {
    password.chars().count() < MIN_PASSWORD
        || PASSWORDS_COMUNES.contains(&password.to_lowercase().as_str())
}

pub fn hash_password(password: &str) -> Result<String> {
    // Argon2id con parámetros EXPLÍCITOS: `default()` es m=19 MiB, t=2, p=1 (el mínimo del
    // RFC 9106, no el recomendado). m=64 MiB · t=3 · p=4 es el mismo costo que la KEK del
    // vault (sync2): ~50 ms acá y ~50 ms por candidato del atacante.
    let parametros = argon2::Params::new(65_536, 3, 4, Some(32))
        .map_err(|error| ServerError::Invalid(format!("parámetros de Argon2 inválidos: {error}")))?;
    let argon = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, parametros);
    argon
        .hash_password(password.as_bytes())
        .map(|phc| phc.to_string())
        .map_err(|error| {
            ServerError::Invalid(format!("no se pudo hashear la password: {error}"))
        })
}

/// Password de prueba compartida por los tests del crate. Va acá para que exista en un solo lugar.
#[cfg(test)]
pub(crate) const PASSWORD_DE_PRUEBA: &str = "password-de-prueba";

/// PHC real y estable para los tests que necesitan una cuenta pero no ejercitan el login.
/// Se calcula una sola vez por proceso (argon2 con m=19 MiB tarda ~50 ms) y lo comparten los
/// helpers de tests de este crate (delta.rs, sync/commit.rs).
#[cfg(test)]
pub(crate) fn phc_de_prueba() -> &'static str {
    static PHC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PHC.get_or_init(|| hash_password(PASSWORD_DE_PRUEBA).expect("el hash de prueba no puede fallar"))
}

pub fn verificar_password(password: &str, phc: &str) -> Result<bool> {
    let parsed = match PasswordHash::new(phc) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(false),
    };

    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

fn hash_senuelo() -> &'static str {
    static HASH_SENUELO: OnceLock<String> = OnceLock::new();

    HASH_SENUELO.get_or_init(|| {
        hash_password("password-senuelo-no-utilizable")
            .expect("Argon2 debe poder generar el hash señuelo")
    })
}

pub async fn crear_cuenta(
    pool: &SqlitePool,
    email: &str,
    password: &str,
) -> Result<Cuenta> {
    let email = normalize_email(email);
    let password_hash = hash_password(password)?;

    let fila = sqlx::query_as::<_, FilaCuenta>(
        "INSERT INTO accounts (email, password_hash)
         VALUES (?, ?)
         RETURNING id, email",
    )
    .bind(&email)
    .bind(password_hash)
    .fetch_one(pool)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            ServerError::Invalid("la cuenta ya existe".to_owned())
        } else {
            ServerError::Db(error)
        }
    })?;

    Ok(Cuenta {
        id: fila.id,
        email: fila.email,
    })
}

/// Crea la PRIMERA cuenta del servidor (bootstrap de `/setup`, ADR-0016). A diferencia de
/// `crear_cuenta`, falla si ya existe CUALQUIER cuenta: la condición vive DENTRO del INSERT
/// (`WHERE NOT EXISTS`) para que dos envíos simultáneos no puedan crear dos cuentas — SQLite
/// serializa a los escritores, el segundo ve el COMMIT del primero, el `INSERT … SELECT`
/// afecta 0 filas y no hay `RETURNING`.
pub async fn crear_cuenta_si_vacio(
    pool: &SqlitePool,
    email: &str,
    password: &str,
) -> Result<Cuenta> {
    let email = normalize_email(email);
    let password_hash = hash_password(password)?;

    // La cuenta del bootstrap nace 'admin' (ADR-0017): es quien operó el server.
    let fila = sqlx::query_as::<_, FilaCuenta>(
        "INSERT INTO accounts (email, password_hash, role)
         SELECT ?, ?, 'admin' WHERE NOT EXISTS (SELECT 1 FROM accounts)
         RETURNING id, email",
    )
    .bind(&email)
    .bind(password_hash)
    .fetch_optional(pool)
    .await
    .map_err(ServerError::Db)?;

    fila.map(|fila| Cuenta {
        id: fila.id,
        email: fila.email,
    })
    .ok_or_else(|| ServerError::Invalid("el servidor ya tiene una cuenta".to_owned()))
}

pub async fn iniciar_sesion(
    pool: &SqlitePool,
    email: &str,
    password: &str,
    name: &str,
    platform: Option<&str>,
) -> Result<(String, i64)> {
    let email = normalize_email(email);

    let fila = sqlx::query_as::<_, FilaCuentaLogin>(
        "SELECT id, password_hash, disabled_at
         FROM accounts
         WHERE email = ?",
    )
    .bind(&email)
    .fetch_optional(pool)
    .await?;

    let (account_id, password_hash) = match fila {
        Some(fila) => (
            // Una cuenta deshabilitada no crea devices ni con la password correcta.
            if fila.disabled_at.is_none() {
                Some(fila.id)
            } else {
                None
            },
            fila.password_hash,
        ),
        None => (None, hash_senuelo().to_owned()),
    };

    let password_valida = verificar_password(password, &password_hash)?;

    let account_id = match (account_id, password_valida) {
        (Some(account_id), true) => account_id,
        _ => return Err(ServerError::Unauthorized),
    };

    let token = emitir_device(pool, account_id, name, platform).await?;
    let device_id = verificar_token(pool, &token).await?;

    Ok((token, device_id))
}

/// Cuenta autenticada por password de CUENTA (base del login del panel web, ADR-0017).
#[derive(Debug, Clone)]
pub struct CuentaAutenticada {
    pub id: i64,
    pub email: String,
    pub role: String,
}

#[derive(Debug, FromRow)]
struct FilaCuentaCompleta {
    id: i64,
    email: String,
    password_hash: String,
    role: String,
    disabled_at: Option<String>,
}

/// Autentica email + password de CUENTA sin tocar devices (el panel no emite un device
/// por cada login del navegador). Mismo señuelo que `iniciar_sesion`: un email inexistente
/// cuesta un Argon2 igual, así que no se filtra por timing qué cuentas existen.
pub async fn autenticar_cuenta(
    pool: &SqlitePool,
    email: &str,
    password: &str,
) -> Result<Option<CuentaAutenticada>> {
    let email = normalize_email(email);

    let fila = sqlx::query_as::<_, FilaCuentaCompleta>(
        "SELECT id, email, password_hash, role, disabled_at
         FROM accounts
         WHERE email = ?",
    )
    .bind(&email)
    .fetch_optional(pool)
    .await?;

    let (autenticada, password_hash) = match fila {
        Some(fila) => (
            // Una cuenta deshabilitada no autentica ni con la password correcta.
            if fila.disabled_at.is_none() {
                Some(CuentaAutenticada {
                    id: fila.id,
                    email: fila.email,
                    role: fila.role,
                })
            } else {
                None
            },
            fila.password_hash,
        ),
        None => (None, hash_senuelo().to_owned()),
    };

    let password_valida = verificar_password(password, &password_hash)?;

    Ok(match (autenticada, password_valida) {
        (Some(cuenta), true) => Some(cuenta),
        _ => None,
    })
}

pub async fn emitir_device(
    pool: &SqlitePool,
    account_id: i64,
    name: &str,
    platform: Option<&str>,
) -> Result<String> {
    let token = token_string(&token_bytes());
    let token_hash = hash_token(&token);
    let ttl_hours = crate::config::ttl_token_horas();
    let expires_modifier = format!("+{ttl_hours} hours");

    let mut transaction = pool.begin().await?;

    let device_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO devices (account_id, name, platform)
         VALUES (?, ?, ?)
         RETURNING id",
    )
    .bind(account_id)
    .bind(name)
    .bind(platform)
    .fetch_one(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT INTO device_tokens
             (token_hash, device_id, account_id, expires_at)
         VALUES (?, ?, ?, datetime('now', ?))",
    )
    .bind(token_hash)
    .bind(device_id)
    .bind(account_id)
    .bind(expires_modifier)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;

    Ok(token)
}

pub async fn verificar_token(pool: &SqlitePool, token: &str) -> Result<i64> {
    let token_hash = hash_token(token);

    let fila = sqlx::query_as::<_, FilaToken>(
        "SELECT t.device_id
         FROM device_tokens AS t
         INNER JOIN devices AS d ON d.id = t.device_id
         WHERE t.token_hash = ?
           AND t.revoked_at IS NULL
           AND d.revoked_at IS NULL
           AND t.expires_at > datetime('now')",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;

    fila.map(|row| row.device_id)
        .ok_or(ServerError::Unauthorized)
}

pub async fn revocar_device(pool: &SqlitePool, device_id: i64) -> Result<()> {
    let mut transaction = pool.begin().await?;

    sqlx::query(
        "UPDATE devices
         SET revoked_at = datetime('now')
         WHERE id = ?",
    )
    .bind(device_id)
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "UPDATE device_tokens
         SET revoked_at = datetime('now')
         WHERE device_id = ?",
    )
    .bind(device_id)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(())
}

pub async fn emitir_challenge(
    pool: &SqlitePool,
    device_id: i64,
    vault_id: i64,
) -> Result<Vec<u8>> {
    // Barrido perezoso de vencidos (el patrón de `uploads::barrer`, y para eso está el
    // índice `idx_challenges_expira`): cada `hello` emite un challenge y sin barrido la
    // tabla crece sin techo con clientes que nunca los consumen. Es higiene: si el DELETE
    // falla, la emisión sigue igual.
    let _ = sqlx::query("DELETE FROM auth_challenges WHERE expires_at < datetime('now')")
        .execute(pool)
        .await;

    let challenge = token_bytes();
    let ttl_seconds = crate::config::ttl_challenge_segundos();
    let expires_modifier = format!("+{ttl_seconds} seconds");

    sqlx::query(
        "INSERT INTO auth_challenges
             (device_id, vault_id, challenge, expires_at)
         VALUES (?, ?, ?, datetime('now', ?))",
    )
    .bind(device_id)
    .bind(vault_id)
    .bind(&challenge)
    .bind(expires_modifier)
    .execute(pool)
    .await?;

    Ok(challenge)
}

pub async fn challenge_pendiente(
    pool: &SqlitePool,
    device_id: i64,
    vault_id: i64,
) -> Result<Option<Vec<u8>>> {
    let challenge = sqlx::query_scalar::<_, Vec<u8>>(
        "SELECT challenge
         FROM auth_challenges
         WHERE device_id = ?
           AND vault_id = ?
           AND used_at IS NULL
           AND expires_at > datetime('now')
         ORDER BY id DESC
         LIMIT 1",
    )
    .bind(device_id)
    .bind(vault_id)
    .fetch_optional(pool)
    .await?;

    Ok(challenge)
}

pub async fn consumir_challenge(
    pool: &SqlitePool,
    device_id: i64,
    vault_id: i64,
    challenge: &[u8],
) -> Result<()> {
    let resultado = sqlx::query(
        "UPDATE auth_challenges
         SET used_at = datetime('now')
         WHERE device_id = ?
           AND vault_id = ?
           AND challenge = ?
           AND used_at IS NULL
           AND expires_at > datetime('now')",
    )
    .bind(device_id)
    .bind(vault_id)
    .bind(challenge)
    .execute(pool)
    .await?;

    if resultado.rows_affected() != 1 {
        return Err(ServerError::Unauthorized);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TestContext {
        pool: SqlitePool,
        path: PathBuf,
        account_id: i64,
        device_id: i64,
        token: String,
        vault_id: i64,
    }

    impl TestContext {
        async fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
            let path = std::env::temp_dir().join(format!(
                "ovaultd-server-auth-{}.db",
                Uuid::new_v4()
            ));

            let pool = crate::db::connect(
                path.to_str()
                    .ok_or("la ruta temporal no es UTF-8")?,
            )
            .await?;

            sqlx::migrate!("../../migrations").run(&pool).await?;

            let cuenta =
                crear_cuenta(&pool, "test@example.com", "password-de-prueba").await?;
            let token = emitir_device(&pool, cuenta.id, "test-device", Some("test")).await?;

            let vault_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO vaults
                     (account_id, name, salt, kdf_params, wrapped_key_pw,
                      wrapped_key_rc, vault_pubkey)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 RETURNING id",
            )
            .bind(cuenta.id)
            .bind("test-vault")
            .bind(vec![0u8; 16])
            .bind("{}")
            .bind(vec![0u8; 32])
            .bind(vec![0u8; 32])
            .bind(vec![0u8; 32])
            .fetch_one(&pool)
            .await?;

            let device_id = verificar_token(&pool, &token).await?;

            Ok(Self {
                pool,
                path,
                account_id: cuenta.id,
                device_id,
                token,
                vault_id,
            })
        }
    }

    impl Drop for TestContext {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[tokio::test]
    async fn challenge_replay_is_rejected(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;

        let challenge =
            emitir_challenge(&context.pool, context.device_id, context.vault_id).await?;

        consumir_challenge(
            &context.pool,
            context.device_id,
            context.vault_id,
            &challenge,
        )
        .await?;

        let replay = consumir_challenge(
            &context.pool,
            context.device_id,
            context.vault_id,
            &challenge,
        )
        .await;

        assert!(matches!(replay, Err(ServerError::Unauthorized)));
        Ok(())
    }

    #[tokio::test]
    async fn revoked_device_cannot_download(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;

        revocar_device(&context.pool, context.device_id).await?;

        let result = verificar_token(&context.pool, &context.token).await;

        assert!(matches!(result, Err(ServerError::Unauthorized)));
        Ok(())
    }

    #[tokio::test]
    async fn token_vencido_es_rechazado(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;
        let token_hash = hash_token(&context.token);

        sqlx::query(
            "UPDATE device_tokens
             SET expires_at = ?
             WHERE token_hash = ?",
        )
        .bind("2000-01-01 00:00:00")
        .bind(token_hash)
        .execute(&context.pool)
        .await?;

        let result = verificar_token(&context.pool, &context.token).await;

        assert!(matches!(result, Err(ServerError::Unauthorized)));
        Ok(())
    }

    #[tokio::test]
    async fn crear_cuenta_duplicada_da_invalid(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;

        let result =
            crear_cuenta(&context.pool, "test@example.com", "password-de-prueba").await;

        assert!(matches!(result, Err(ServerError::Invalid(_))));
        Ok(())
    }

    #[tokio::test]
    async fn password_correcta_da_token_y_device_id(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;

        let (token, device_id) = iniciar_sesion(
            &context.pool,
            " TEST@example.com ",
            "password-de-prueba",
            "login-device",
            Some("test"),
        )
        .await?;

        let verified_device_id = verificar_token(&context.pool, &token).await?;
        assert_eq!(device_id, verified_device_id);

        let row_device_id = sqlx::query_scalar::<_, i64>(
            "SELECT id
             FROM devices
             WHERE account_id = ?
               AND id = ?",
        )
        .bind(context.account_id)
        .bind(device_id)
        .fetch_one(&context.pool)
        .await?;

        assert_eq!(device_id, row_device_id);
        Ok(())
    }

    #[tokio::test]
    async fn password_incorrecta_da_unauthorized(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;

        let result = iniciar_sesion(
            &context.pool,
            "test@example.com",
            "password-incorrecta",
            "login-device",
            None,
        )
        .await;

        assert!(matches!(result, Err(ServerError::Unauthorized)));
        Ok(())
    }

    #[tokio::test]
    async fn email_inexistente_da_el_mismo_error_que_password_incorrecta(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;

        let password_incorrecta = iniciar_sesion(
            &context.pool,
            "test@example.com",
            "password-incorrecta",
            "login-device",
            None,
        )
        .await
        .expect_err("la password incorrecta debe fallar");

        let email_inexistente = iniciar_sesion(
            &context.pool,
            "inexistente@example.com",
            "password-incorrecta",
            "login-device",
            None,
        )
        .await
        .expect_err("el email inexistente debe fallar");

        assert_eq!(password_incorrecta.to_string(), email_inexistente.to_string());
        assert_eq!(
            format!("{password_incorrecta:?}"),
            format!("{email_inexistente:?}")
        );
        Ok(())
    }

    #[tokio::test]
    async fn el_hash_guardado_no_contiene_la_password_en_claro(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;
        let password = "password-de-prueba";

        let password_hash = sqlx::query_scalar::<_, String>(
            "SELECT password_hash
             FROM accounts
             WHERE id = ?",
        )
        .bind(context.account_id)
        .fetch_one(&context.pool)
        .await?;

        assert!(password_hash.starts_with("$argon2id$"));
        assert!(!password_hash.contains(password));
        assert!(!password_hash.contains("password"));
        assert!(verificar_password(password, &password_hash)?);
        Ok(())
    }

    #[tokio::test]
    async fn el_phc_de_prueba_verifica_con_su_password(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        assert!(verificar_password(PASSWORD_DE_PRUEBA, phc_de_prueba())?);
        assert!(!verificar_password("otra-cosa", phc_de_prueba())?);
        Ok(())
    }

    #[tokio::test]
    async fn dos_hashes_de_la_misma_password_son_distintos(
        ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let context = TestContext::new().await?;
        let password = "password-de-prueba";

        let cuenta_1 =
            crear_cuenta(&context.pool, "primera@example.com", password).await?;
        let cuenta_2 =
            crear_cuenta(&context.pool, "segunda@example.com", password).await?;

        let hash_1 = sqlx::query_scalar::<_, String>(
            "SELECT password_hash
             FROM accounts
             WHERE id = ?",
        )
        .bind(cuenta_1.id)
        .fetch_one(&context.pool)
        .await?;

        let hash_2 = sqlx::query_scalar::<_, String>(
            "SELECT password_hash
             FROM accounts
             WHERE id = ?",
        )
        .bind(cuenta_2.id)
        .fetch_one(&context.pool)
        .await?;

        assert_ne!(hash_1, hash_2);
        assert!(verificar_password(password, &hash_1)?);
        assert!(verificar_password(password, &hash_2)?);
        Ok(())
    }
}

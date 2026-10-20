//! Sesiones web del panel (ADR-0017): cookie opaca con CSRF, auditoría y freno de intentos.
//!
//! No son `device_tokens`: la cookie solo abre `/admin` (no sirve para el API ni el WS),
//! viaja `HttpOnly` + `SameSite=Lax`, y cada POST exige además el token CSRF de la sesión.
//! El token de sesión crudo solo vive en la cookie (en la base queda su SHA-256); el token
//! CSRF se guarda crudo porque tiene que viajar en el HTML de cada formulario.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sqlx::SqlitePool;

use crate::auth;
use crate::error::Result;

/// Días de vida de una sesión web.
pub const SESION_TTL_DIAS: i64 = 30;
/// Nombre de la cookie de sesión.
pub const COOKIE_SESION: &str = "ovaultd_session";
/// Nombre del campo oculto de CSRF en los formularios.
pub const CAMPO_CSRF: &str = "csrf";

/// Sesión recién creada: los tokens crudos viajan UNA vez (cookie + form); la base guarda
/// el hash del de sesión y el CSRF tal cual (tiene que poder reimprimirse en el HTML).
pub struct SesionCreada {
    pub token: String,
    pub csrf: String,
}

/// Sesión válida, con lo necesario para autorizar y renderizar.
pub struct SesionActiva {
    pub session_id: i64,
    pub account_id: i64,
    pub email: String,
    pub role: String,
    pub csrf_token: String,
}

pub async fn crear_sesion(
    pool: &SqlitePool,
    account_id: i64,
    ip: Option<&str>,
    user_agent: Option<&str>,
) -> Result<SesionCreada> {
    let token = auth::token_string(&auth::token_bytes());
    let csrf = auth::token_string(&auth::token_bytes());
    let expires_modifier = format!("+{SESION_TTL_DIAS} days");

    sqlx::query(
        "INSERT INTO sessions (token_hash, csrf_token, account_id, expires_at, ip, user_agent)
         VALUES (?, ?, ?, datetime('now', ?), ?, ?)",
    )
    .bind(auth::hash_token(&token))
    .bind(&csrf)
    .bind(account_id)
    .bind(expires_modifier)
    .bind(ip)
    .bind(user_agent)
    .execute(pool)
    .await?;

    Ok(SesionCreada { token, csrf })
}

/// Verifica una cookie de sesión y deja el rastro de actividad al día (`last_seen`).
pub async fn verificar_sesion(pool: &SqlitePool, token: &str) -> Result<Option<SesionActiva>> {
    let fila = sqlx::query_as::<_, FilaSesion>(
        "SELECT s.id, s.account_id, s.csrf_token, a.email, a.role
         FROM sessions s
         JOIN accounts a ON a.id = s.account_id
         WHERE s.token_hash = ? AND datetime('now') < s.expires_at AND a.disabled_at IS NULL",
    )
    .bind(auth::hash_token(token))
    .fetch_optional(pool)
    .await?;

    let Some(fila) = fila else {
        return Ok(None);
    };

    sqlx::query("UPDATE sessions SET last_seen = datetime('now') WHERE id = ?")
        .bind(fila.id)
        .execute(pool)
        .await?;

    Ok(Some(SesionActiva {
        session_id: fila.id,
        account_id: fila.account_id,
        email: fila.email,
        role: fila.role,
        csrf_token: fila.csrf_token,
    }))
}

/// El token CSRF del formulario tiene que ser el de ESTA sesión (tiempo constante).
pub fn csrf_valido(sesion: &SesionActiva, csrf_recibido: &str) -> bool {
    auth::tokens_iguales(csrf_recibido, &sesion.csrf_token)
}

pub async fn revocar_sesion(pool: &SqlitePool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
        .bind(auth::hash_token(token))
        .execute(pool)
        .await?;

    Ok(())
}

/// Revoca TODAS las sesiones web de una cuenta (deshabilitar, borrar, reset de password).
pub async fn revocar_todas(pool: &SqlitePool, account_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE account_id = ?")
        .bind(account_id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Revoca todas las sesiones de la cuenta EXCEPTO la actual (cambio de password).
pub async fn revocar_otras_sesiones(
    pool: &SqlitePool,
    account_id: i64,
    token_actual: &str,
) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE account_id = ? AND token_hash != ?")
        .bind(account_id)
        .bind(auth::hash_token(token_actual))
        .execute(pool)
        .await?;

    Ok(())
}

/// Limpieza perezosa: borra las sesiones vencidas (se llama en cada login).
pub async fn limpiar_vencidas(pool: &SqlitePool) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE datetime('now') >= expires_at")
        .execute(pool)
        .await?;

    Ok(())
}

/// Auditoría best-effort: si no se puede escribir el asiento, se loguea y sigue — la
/// acción del usuario ya ocurrió y perder el log no puede tumbar la respuesta.
pub async fn auditar(
    pool: &SqlitePool,
    actor_account_id: Option<i64>,
    action: &str,
    detail: Option<&str>,
    ip: Option<&str>,
) {
    let resultado = sqlx::query(
        "INSERT INTO audit_log (actor_account_id, action, detail, ip) VALUES (?, ?, ?, ?)",
    )
    .bind(actor_account_id)
    .bind(action)
    .bind(detail)
    .bind(ip)
    .execute(pool)
    .await;

    if let Err(error) = resultado {
        tracing::warn!(%error, action, "no pude escribir el asiento de auditoría");
    }
}

#[derive(Debug, sqlx::FromRow)]
struct FilaSesion {
    id: i64,
    account_id: i64,
    csrf_token: String,
    email: String,
    role: String,
}

// ------------------------------------------------------------------ freno de intentos

/// Freno de fuerza bruta en memoria (single-node por diseño): N intentos por ventana.
pub struct LimitadorDeLogin {
    intentos: Mutex<HashMap<String, VecDeque<Instant>>>,
    max: usize,
    ventana: Duration,
}

impl LimitadorDeLogin {
    pub fn nuevo(max: usize, ventana: Duration) -> Self {
        Self {
            intentos: Mutex::new(HashMap::new()),
            max,
            ventana,
        }
    }

    /// ¿Le queda margen a esta clave (típicamente ip|email)?
    pub fn permitir(&self, clave: &str) -> bool {
        let mut intentos = self.intentos.lock().unwrap_or_else(|poison| poison.into_inner());
        let cola = intentos.entry(clave.to_owned()).or_default();
        let corte = Instant::now() - self.ventana;
        while cola.front().is_some_and(|instante| *instante < corte) {
            cola.pop_front();
        }

        cola.len() < self.max
    }

    /// Registra un intento (fallido o no: la ventana se corre igual).
    pub fn registrar(&self, clave: &str) {
        let mut intentos = self.intentos.lock().unwrap_or_else(|poison| poison.into_inner());
        intentos
            .entry(clave.to_owned())
            .or_default()
            .push_back(Instant::now());
    }

    /// Login exitoso: se limpia la clave.
    pub fn limpiar(&self, clave: &str) {
        let mut intentos = self.intentos.lock().unwrap_or_else(|poison| poison.into_inner());
        intentos.remove(clave);
    }
}

impl Default for LimitadorDeLogin {
    fn default() -> Self {
        Self::nuevo(5, Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csrf_contra_la_sesion() {
        let csrf = "csrf-de-prueba";
        let sesion = SesionActiva {
            session_id: 1,
            account_id: 1,
            email: "a@b.c".to_owned(),
            role: "admin".to_owned(),
            csrf_token: csrf.to_owned(),
        };

        assert!(csrf_valido(&sesion, csrf));
        assert!(!csrf_valido(&sesion, "otro"));
        assert!(!csrf_valido(&sesion, ""));
        assert!(!csrf_valido(&sesion, &csrf.to_uppercase()));
    }

    #[test]
    fn el_limitador_frena_y_se_limpia() {
        let limitador = LimitadorDeLogin::nuevo(3, Duration::from_secs(60));

        assert!(limitador.permitir("yo@ejemplo.com"));
        limitador.registrar("yo@ejemplo.com");
        limitador.registrar("yo@ejemplo.com");
        assert!(limitador.permitir("yo@ejemplo.com"));
        limitador.registrar("yo@ejemplo.com");
        assert!(!limitador.permitir("yo@ejemplo.com"));

        // otra clave no se contamina
        assert!(limitador.permitir("otro@ejemplo.com"));

        // un login exitoso limpia
        limitador.limpiar("yo@ejemplo.com");
        assert!(limitador.permitir("yo@ejemplo.com"));
    }

    #[test]
    fn la_ventana_vieja_se_descarta() {
        let limitador = LimitadorDeLogin::nuevo(1, Duration::from_millis(1));
        limitador.registrar("k");
        assert!(!limitador.permitir("k"));

        // Bajo carga (los tests de Argon2 saturan la CPU) el hilo puede no ser schedulado
        // por más de un segundo: se reintenta con un techo amplio, midiendo por reloj.
        let inicio = std::time::Instant::now();
        let mut permitido = false;
        while inicio.elapsed() < Duration::from_secs(5) {
            if limitador.permitir("k") {
                permitido = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(permitido, "la ventana de 1 ms no expiró en 5 s");
    }
}

use crate::error::{Result, ServerError};
use std::{fmt, path::PathBuf};

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: String,
    pub database_path: String,
    pub cas_root: PathBuf,
    pub token_ttl_hours: i64,
    pub challenge_ttl_seconds: i64,
    /// TTL de una subida reanudable sin actividad (horas): vencida, el barrido la borra y
    /// libera su reserva de cuota (ADR-0012).
    pub upload_ttl_hours: i64,
    /// Token de instalación del bootstrap por web (`/setup`, ADR-0016). Seteado, el formulario
    /// de la primera cuenta exige `?token=…`; ausente/vacío, queda abierto MIENTRAS el servidor
    /// no tenga ninguna cuenta. Nunca se imprime en logs (el `Display` solo dice set/unset).
    pub setup_token: Option<String>,
    /// Cookie `Secure` del panel web (ADR-0017): `auto` la marca cuando el request llega por
    /// HTTPS (X-Forwarded-Proto del borde), `on`/`off` la fuerzan.
    pub secure_cookies: ModoSecureCookies,
    /// Confiar en los headers de un proxy inverso (`X-Forwarded-Proto`, `X-Real-IP`,
    /// `X-Forwarded-For`). Default `false`: los headers de CUALQUIERA no se obedecen — la IP
    /// del cliente sale del peer TCP (y sin borde no hay señal de HTTPS). Activalo SOLO
    /// cuando hay un borde de confianza adelante (ej. NPM/nginx en el mismo host): si no,
    /// la IP del cliente y el `Secure` de la cookie quedan a merced de quien mande el header.
    pub trust_proxy: bool,
    /// Exposición de `GET /metrics` (F-09 de la auditoría): `off` no monta la ruta (404),
    /// `localhost` responde solo al peer TCP de loopback (acceso directo al puerto, sin
    /// borde) y `on` la expone como el resto del API (restringila en el borde). Default
    /// `off`: las métricas exponen la disponibilidad del proceso y no tienen por qué ser
    /// públicas.
    pub metrics: ModoMetrics,
}

/// Modo de exposición de `/metrics`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModoMetrics {
    Off,
    Localhost,
    On,
}

/// Modo de la marca `Secure` de la cookie de sesión del panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModoSecureCookies {
    Auto,
    Siempre,
    Nunca,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::construir(|name| std::env::var(name).ok())
    }

    fn construir<F>(get: F) -> Result<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let bind_addr = valor(&get, "OVAULTD_BIND")
            .unwrap_or_else(|| "127.0.0.1:8080".to_owned());

        let database_path = valor(&get, "OVAULTD_DB")
            .unwrap_or_else(|| "./.tmp/server/metadata.db".to_owned());

        let cas_root = PathBuf::from(
            valor(&get, "OVAULTD_CAS")
                .unwrap_or_else(|| "./.tmp/server/cas".to_owned()),
        );

        let token_ttl_hours = leer_entero(
            &get,
            "OVAULTD_TOKEN_TTL_HOURS",
            720,
        )?;

        let challenge_ttl_seconds = leer_entero(
            &get,
            "OVAULTD_CHALLENGE_TTL",
            300,
        )?;

        let upload_ttl_hours = leer_entero(
            &get,
            "OVAULTD_UPLOAD_TTL_HOURS",
            24,
        )?;

        let setup_token = valor(&get, "OVAULTD_SETUP_TOKEN");

        let secure_cookies = match valor(&get, "OVAULTD_SECURE_COOKIES").as_deref() {
            None | Some("auto") => ModoSecureCookies::Auto,
            Some("on") => ModoSecureCookies::Siempre,
            Some("off") => ModoSecureCookies::Nunca,
            Some(otro) => {
                return Err(ServerError::Invalid(format!(
                    "OVAULTD_SECURE_COOKIES tiene un valor inválido: {otro:?} (auto|on|off)"
                )))
            }
        };

        let trust_proxy = match valor(&get, "OVAULTD_TRUST_PROXY").as_deref() {
            None | Some("0") | Some("false") | Some("off") => false,
            Some("1") | Some("true") | Some("on") => true,
            Some(otro) => {
                return Err(ServerError::Invalid(format!(
                    "OVAULTD_TRUST_PROXY tiene un valor inválido: {otro:?} (0|1)"
                )))
            }
        };

        let metrics = match valor(&get, "OVAULTD_METRICS").as_deref() {
            None | Some("off") => ModoMetrics::Off,
            Some("localhost") => ModoMetrics::Localhost,
            Some("on") => ModoMetrics::On,
            Some(otro) => {
                return Err(ServerError::Invalid(format!(
                    "OVAULTD_METRICS tiene un valor inválido: {otro:?} (off|localhost|on)"
                )))
            }
        };

        Ok(Self {
            bind_addr,
            database_path,
            cas_root,
            token_ttl_hours,
            challenge_ttl_seconds,
            upload_ttl_hours,
            setup_token,
            secure_cookies,
            trust_proxy,
            metrics,
        })
    }
}

impl fmt::Display for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "bind_addr={}, database_path={}, cas_root={}, token_ttl_hours={}, challenge_ttl_seconds={}, upload_ttl_hours={}, setup_token={}, secure_cookies={:?}, trust_proxy={}",
            self.bind_addr,
            self.database_path,
            self.cas_root.display(),
            self.token_ttl_hours,
            self.challenge_ttl_seconds,
            self.upload_ttl_hours,
            if self.setup_token.is_some() { "set" } else { "unset" },
            self.secure_cookies,
            self.trust_proxy,
        )
    }
}

fn valor<F>(get: &F, name: &str) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    get(name).filter(|value| !value.trim().is_empty())
}

fn leer_entero<F>(get: &F, name: &str, default: i64) -> Result<i64>
where
    F: Fn(&str) -> Option<String>,
{
    let Some(value) = valor(get, name) else {
        return Ok(default);
    };

    let parsed = value.parse::<i64>().map_err(|_| {
        ServerError::Invalid(format!(
            "{name} tiene un valor inválido: {value:?}"
        ))
    })?;

    if parsed <= 0 {
        return Err(ServerError::Invalid(format!(
            "{name} debe ser mayor que cero; se recibió {parsed}"
        )));
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usa_los_defaults_cuando_no_hay_variables() {
        let config = Config::construir(|_| None).unwrap();

        assert_eq!(config.bind_addr, "127.0.0.1:8080");
        assert_eq!(config.database_path, "./.tmp/server/metadata.db");
        assert_eq!(config.cas_root, PathBuf::from("./.tmp/server/cas"));
        assert_eq!(config.token_ttl_hours, 720);
        assert_eq!(config.challenge_ttl_seconds, 300);
        assert_eq!(config.upload_ttl_hours, 24);
        assert!(config.setup_token.is_none());
        assert_eq!(config.secure_cookies, ModoSecureCookies::Auto);
        // Seguro por defecto: sin borde declarado, los headers de un proxy NO se obedecen.
        assert!(!config.trust_proxy);
    }

    #[test]
    fn variable_vacia_usa_el_default() {
        let config = Config::construir(|name| match name {
            "OVAULTD_BIND" => Some(String::new()),
            "OVAULTD_DB" => Some("   ".to_owned()),
            "OVAULTD_CAS" => Some("".to_owned()),
            "OVAULTD_TOKEN_TTL_HOURS" => Some(" ".to_owned()),
            "OVAULTD_CHALLENGE_TTL" => Some("   ".to_owned()),
            "OVAULTD_UPLOAD_TTL_HOURS" => Some(String::new()),
            "OVAULTD_SETUP_TOKEN" => Some("   ".to_owned()),
            _ => None,
        })
        .unwrap();

        assert_eq!(config.bind_addr, "127.0.0.1:8080");
        assert_eq!(config.database_path, "./.tmp/server/metadata.db");
        assert_eq!(config.cas_root, PathBuf::from("./.tmp/server/cas"));
        assert_eq!(config.token_ttl_hours, 720);
        assert_eq!(config.challenge_ttl_seconds, 300);
        assert_eq!(config.upload_ttl_hours, 24);
        assert!(config.setup_token.is_none());
    }

    #[test]
    fn setup_token_se_lee_de_la_variable() {
        let config = Config::construir(|name| match name {
            "OVAULTD_SETUP_TOKEN" => Some("token-de-instalacion".to_owned()),
            _ => None,
        })
        .unwrap();

        assert_eq!(config.setup_token.as_deref(), Some("token-de-instalacion"));
        // el Display nunca imprime el valor: solo set/unset
        assert!(config.to_string().contains("setup_token=set"));
        assert!(!config.to_string().contains("token-de-instalacion"));
    }

    #[test]
    fn numero_invalido_da_error_con_el_nombre_de_la_variable() {
        let resultado = Config::construir(|name| match name {
            "OVAULTD_TOKEN_TTL_HOURS" => Some("no-es-un-numero".to_owned()),
            _ => None,
        });

        let error = resultado.unwrap_err();
        let mensaje = error.to_string();

        assert!(mensaje.contains("OVAULTD_TOKEN_TTL_HOURS"));
        assert!(mensaje.contains("no-es-un-numero"));
    }

    #[test]
    fn ttl_cero_es_rechazado() {
        let resultado = Config::construir(|name| match name {
            "OVAULTD_CHALLENGE_TTL" => Some("0".to_owned()),
            _ => None,
        });

        let error = resultado.unwrap_err();
        let mensaje = error.to_string();

        assert!(mensaje.contains("OVAULTD_CHALLENGE_TTL"));
        assert!(mensaje.contains("mayor que cero"));
    }

    #[test]
    fn secure_cookies_se_lee_y_valida() {
        let on = Config::construir(|name| match name {
            "OVAULTD_SECURE_COOKIES" => Some("on".to_owned()),
            _ => None,
        })
        .unwrap();
        assert_eq!(on.secure_cookies, ModoSecureCookies::Siempre);

        let malo = Config::construir(|name| match name {
            "OVAULTD_SECURE_COOKIES" => Some("quizas".to_owned()),
            _ => None,
        });
        assert!(malo
            .unwrap_err()
            .to_string()
            .contains("OVAULTD_SECURE_COOKIES"));
    }

    #[test]
    fn trust_proxy_se_lee_y_valida() {
        // Default: no se confía en headers de un proxy.
        let por_defecto = Config::construir(|_| None).unwrap();
        assert!(!por_defecto.trust_proxy);

        for valor in ["1", "true", "on"] {
            let activo = Config::construir(|name| match name {
                "OVAULTD_TRUST_PROXY" => Some(valor.to_owned()),
                _ => None,
            })
            .unwrap();
            assert!(activo.trust_proxy, "{valor} debe activarlo");
        }

        for valor in ["0", "false", "off"] {
            let inactivo = Config::construir(|name| match name {
                "OVAULTD_TRUST_PROXY" => Some(valor.to_owned()),
                _ => None,
            })
            .unwrap();
            assert!(!inactivo.trust_proxy, "{valor} debe desactivarlo");
        }

        let malo = Config::construir(|name| match name {
            "OVAULTD_TRUST_PROXY" => Some("quizas".to_owned()),
            _ => None,
        });
        assert!(malo
            .unwrap_err()
            .to_string()
            .contains("OVAULTD_TRUST_PROXY"));
    }
}

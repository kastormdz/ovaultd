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

        Ok(Self {
            bind_addr,
            database_path,
            cas_root,
            token_ttl_hours,
            challenge_ttl_seconds,
            upload_ttl_hours,
        })
    }
}

impl fmt::Display for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "bind_addr={}, database_path={}, cas_root={}, token_ttl_hours={}, challenge_ttl_seconds={}, upload_ttl_hours={}",
            self.bind_addr,
            self.database_path,
            self.cas_root.display(),
            self.token_ttl_hours,
            self.challenge_ttl_seconds,
            self.upload_ttl_hours,
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
            _ => None,
        })
        .unwrap();

        assert_eq!(config.bind_addr, "127.0.0.1:8080");
        assert_eq!(config.database_path, "./.tmp/server/metadata.db");
        assert_eq!(config.cas_root, PathBuf::from("./.tmp/server/cas"));
        assert_eq!(config.token_ttl_hours, 720);
        assert_eq!(config.challenge_ttl_seconds, 300);
        assert_eq!(config.upload_ttl_hours, 24);
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
}

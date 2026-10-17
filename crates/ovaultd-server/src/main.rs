fn main() {
    iniciar_logs();

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error al crear el runtime de Tokio: {error}");
            std::process::exit(1);
        }
    };

    if let Err(error) = runtime.block_on(iniciar_servidor()) {
        eprintln!("error al arrancar ovaultd: {error}");
        std::process::exit(1);
    }
}

use ovaultd_server::{config::Config, db, routes, storage::CasStore};
use tracing_subscriber::EnvFilter;

/// Logs: texto legible por defecto; JSON con `OVAULTD_LOG_JSON=1` (lo que usa el compose).
/// Nivel: `OVAULTD_LOG` (EnvFilter; default `info`). Nunca se loguean tokens ni payloads.
fn iniciar_logs() {
    let filtro = EnvFilter::try_from_env("OVAULTD_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let json = matches!(
        std::env::var("OVAULTD_LOG_JSON").as_deref(),
        Ok("1") | Ok("true")
    );

    let builder = tracing_subscriber::fmt().with_env_filter(filtro);

    if json {
        builder.json().init();
    } else {
        builder.init();
    }
}

async fn iniciar_servidor() -> Result<(), String> {
    let config =
        Config::from_env().map_err(|error| format!("no se pudo cargar la configuración: {error}"))?;

    tracing::info!(config = %config, "configuración cargada");

    let bind_addr = config.bind_addr.clone();

    let pool = db::connect(&config.database_path)
        .await
        .map_err(|error| format!("no se pudo conectar a la base de datos: {error}"))?;

    db::migrate(&pool)
        .await
        .map_err(|error| format!("no se pudieron ejecutar las migraciones: {error}"))?;

    let cas = CasStore::new(config.cas_root.clone())
        .map_err(|error| format!("no se pudo inicializar el almacenamiento CAS: {error}"))?;

    let estado = routes::EstadoServidor {
        pool,
        config,
        cas: cas.into(),
    };
    let app = routes::router(estado);

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .map_err(|error| format!("no se pudo escuchar en {bind_addr}: {error}"))?;

    tracing::info!(bind_addr = %bind_addr, "ovaultd escuchando");

    axum::serve(listener, app)
        .await
        .map_err(|error| format!("el servidor terminó con un error: {error}"))
}

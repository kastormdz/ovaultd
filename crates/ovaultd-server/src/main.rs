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

    // Los módulos sin acceso al estado (auth) leen los TTL de acá.
    ovaultd_server::config::instalar_global(std::sync::Arc::new(config.clone()));

    let estado = routes::EstadoServidor {
        pool,
        config,
        cas: cas.into(),
        limitador_login: Default::default(),
        notifier: std::sync::Arc::new(ovaultd_server::storage::Notifier::new()),
    };
    let app_pool = estado.pool.clone();
    let app_cas_raiz = estado.config.cas_root.clone();
    let app = routes::router(estado);

    // Barrido periódico de subidas abandonadas: `crear_upload` barre, pero un server en
    // reposo (sin subidas nuevas) acumulaba `.part` y reservas de cuota vencidas para
    // siempre. Una hora; el primer tick NO es inmediato (no compite con el arranque).
    {
        let pool_barrido = app_pool.clone();
        let cas_raiz = app_cas_raiz.clone();
        tokio::spawn(async move {
            let periodo = std::time::Duration::from_secs(3600);
            let mut intervalo = tokio::time::interval_at(tokio::time::Instant::now() + periodo, periodo);
            intervalo.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // F-06: un barrido al arrancar. El ticker solo no corre hasta la primera hora,
            // así que un `.part` huérfano de un crash esperaba 60 minutos.
            match ovaultd_server::uploads::barrer(&pool_barrido, &cas_raiz).await {
                Ok(0) => {}
                Ok(borradas) => tracing::info!(borradas, "barrido de uploads al arrancar"),
                Err(error) => tracing::warn!(%error, "barrido de uploads al arrancar falló"),
            }
            loop {
                intervalo.tick().await;
                match ovaultd_server::uploads::barrer(&pool_barrido, &cas_raiz).await {
                    Ok(0) => {}
                    Ok(borradas) => tracing::info!(borradas, "barrido de subidas abandonadas"),
                    Err(error) => tracing::warn!(%error, "barrido de subidas falló"),
                }
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .map_err(|error| format!("no se pudo escuchar en {bind_addr}: {error}"))?;

    // TCP_NODELAY en el socket de ESCUCHA (los aceptados heredan el flag en Linux): con
    // Nagle activo, cada request chico sobre loopback paga ~40 ms de delayed ACK — 1000
    // PUT de blobs = 40 s de espera pura. Es el cuello REAL del bench de subida.
    // SAFETY: el fd es válido (listener vivo) y el valor es un i32 de 1.
    {
        use std::os::fd::AsRawFd;
        let uno: libc::c_int = 1;
        let rc = unsafe {
            libc::setsockopt(
                listener.as_raw_fd(),
                libc::IPPROTO_TCP,
                libc::TCP_NODELAY,
                (&uno as *const libc::c_int).cast(),
                std::mem::size_of_val(&uno) as libc::socklen_t,
            )
        };
        if rc != 0 {
            return Err(format!(
                "no se pudo configurar TCP_NODELAY: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    tracing::info!(bind_addr = %bind_addr, "ovaultd escuchando");

    // Con connect info: cada request lleva la IP real del peer TCP (la usa el freno de
    // login y la auditoría cuando NO hay un proxy de confianza adelante).
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .map_err(|error| format!("el servidor terminó con un error: {error}"))
}

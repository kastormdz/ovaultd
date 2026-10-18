//! `ovaultd-admin` — operación del server **sin descifrar nada**.
//!
//! Cuentas, salud e inspección operacional. Nunca recibe passwords de vault ni
//! claves: la única password que toca es la de CUENTA (se lee de stdin y el
//! server la hashea con argon2id — jamás por argumento, jamás en logs).

use clap::{Parser, Subcommand};
use ovaultd_server::{auth, db};
use sqlx::SqlitePool;

#[derive(Parser)]
#[command(
    name = "ovaultd-admin",
    version,
    about = "Administración de ovaultd: cuentas, salud e inspección (sin descifrar contenido)"
)]
struct Cli {
    /// Ruta de la base SQLite (o variable OVAULTD_DB).
    #[arg(long, env = "OVAULTD_DB")]
    db: String,

    #[command(subcommand)]
    comando: Comando,
}

#[derive(Subcommand)]
enum Comando {
    /// Crea una cuenta. La password se lee de stdin (nunca por argumento).
    CreateAccount {
        #[arg(long)]
        email: String,
    },
    /// Comprueba que la base responde.
    Health,
    /// Inspección operacional: conteos y tamaños. Nada de esto descifra contenido.
    Inspect,
    /// GC de blobs huérfanos (fase 5): sin flags es dry-run (solo reporta).
    Gc {
        /// Audita el invariante de referencias contra las fuentes (no escribe).
        #[arg(long)]
        verify: bool,
        /// Borra de verdad: filas + archivos del CAS. Corré el server PARADO.
        #[arg(long)]
        delete: bool,
        /// Limita a un vault (por id).
        #[arg(long)]
        vault: Option<i64>,
        /// Gracia en minutos para blobs recién publicados (default: 1440 = 24 h).
        #[arg(long, default_value_t = ovaultd_server::maintenance::gc::GRACIA_MINUTOS_POR_DEFECTO)]
        gracia_minutos: i64,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let pool = db::connect(&cli.db).await?;

    match cli.comando {
        Comando::CreateAccount { email } => create_account(&pool, &email, &cli.db).await?,
        Comando::Health => health(&pool, &cli.db).await?,
        Comando::Inspect => inspect(&pool).await?,
        Comando::Gc {
            verify,
            delete,
            vault,
            gracia_minutos,
        } => comando_gc(&pool, verify, delete, vault, gracia_minutos).await?,
    }

    Ok(())
}

async fn create_account(
    pool: &SqlitePool,
    email: &str,
    ruta_db: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("Password de la cuenta {email} (se lee de stdin):");
    let mut linea = String::new();
    std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut linea)?;
    let password = linea.trim_end_matches(['\r', '\n']);
    if password.is_empty() {
        return Err("password vacía: nada que hacer".into());
    }

    db::migrate(pool).await?;
    let cuenta = auth::crear_cuenta(pool, email, password).await?;

    println!("cuenta creada: #{} {} (db {})", cuenta.id, cuenta.email, ruta_db);
    Ok(())
}

async fn health(pool: &SqlitePool, ruta_db: &str) -> Result<(), Box<dyn std::error::Error>> {
    let uno: i64 = sqlx::query_scalar("SELECT 1").fetch_one(pool).await?;
    let cuentas: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts")
        .fetch_one(pool)
        .await?;

    println!("health: ok (SELECT {uno}; {cuentas} cuentas; db {ruta_db})");
    Ok(())
}

async fn inspect(pool: &SqlitePool) -> Result<(), Box<dyn std::error::Error>> {
    let cuentas: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts")
        .fetch_one(pool)
        .await?;
    let devices: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices")
        .fetch_one(pool)
        .await?;
    let devices_revocados: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE revoked_at IS NOT NULL")
            .fetch_one(pool)
            .await?;
    let tokens_vivos: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM device_tokens WHERE revoked_at IS NULL AND expires_at > datetime('now')",
    )
    .fetch_one(pool)
    .await?;
    let vaults: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vaults")
        .fetch_one(pool)
        .await?;
    let revisiones: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM revisions")
        .fetch_one(pool)
        .await?;
    let entradas: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files")
        .fetch_one(pool)
        .await?;
    let (blobs, bytes_blob): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(size), 0) FROM blobs")
            .fetch_one(pool)
            .await?;
    let (uploads_pendientes, bytes_pendientes): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(total - offset), 0) FROM uploads WHERE offset < total",
    )
    .fetch_one(pool)
    .await?;

    println!("cuentas:            {cuentas}");
    println!("devices:            {devices} ({devices_revocados} revocados)");
    println!("tokens vivos:       {tokens_vivos}");
    println!("vaults:             {vaults}");
    println!("revisiones:         {revisiones}");
    println!("entradas en files:  {entradas}");
    println!("blobs en el índice: {blobs} ({bytes_blob} B físicos)");
    println!("uploads pendientes: {uploads_pendientes} ({bytes_pendientes} B por confirmar)");
    Ok(())
}

async fn comando_gc(
    pool: &SqlitePool,
    verify: bool,
    delete: bool,
    vault: Option<i64>,
    gracia_minutos: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    use ovaultd_server::maintenance::gc;
    use ovaultd_server::storage::CasStore;

    if verify {
        let discrepancias = gc::verificar(pool, vault).await?;
        if discrepancias.is_empty() {
            println!("verify: invariante sano (refs = revisiones con blob + snapshots)");
        } else {
            println!("verify: {} discrepancia(s):", discrepancias.len());
            for d in &discrepancias {
                println!(
                    "  vault #{} {} refs={} esperado={}",
                    d.vault_id,
                    d.hash_hex(),
                    d.refs,
                    d.esperado
                );
            }
        }
        return Ok(());
    }

    let candidatos = gc::planificar(pool, vault, gracia_minutos).await?;
    let bytes: i64 = candidatos.iter().map(|c| c.size).sum();
    println!(
        "gc: {} candidato(s), {bytes} B {}",
        candidatos.len(),
        if delete {
            "(borrando)"
        } else {
            "(dry-run: solo reporte)"
        }
    );
    for c in &candidatos {
        println!(
            "  vault #{} {} {} B {}",
            c.vault_id,
            c.hash_hex(),
            c.size,
            c.created_at
        );
    }

    if delete {
        let ruta_cas = std::env::var("OVAULTD_CAS").unwrap_or_else(|_| "/data/cas".to_owned());
        let cas = CasStore::new(ruta_cas.into())?;
        let (borrados, bytes) = gc::borrar(pool, &cas, &candidatos).await?;
        println!("gc: borrados {borrados} blob(s), {bytes} B liberados");
    }

    Ok(())
}

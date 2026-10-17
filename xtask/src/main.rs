//! xtask — tareas de construcción del repo (`cargo xtask <tarea>`).
//!
//! Fase 1 implementa `protocol-check`: el gate de conformidad v2, que ejecuta el
//! MISMO fixture (`tests/fixtures/protocol/v2.json`) desde Rust y desde TypeScript.
//! Las tareas de fases posteriores (`e2e`, `backup-restore`, `bench`, `chaos`) se
//! agregan acá cuando existan sus harness.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None | Some("help") | Some("--help") | Some("-h") => {
            ayuda();
            ExitCode::SUCCESS
        }
        Some("protocol-check") => protocol_check(),
        Some("backup-restore") => backup_restore(),
        Some(otro) => {
            eprintln!("xtask: tarea desconocida `{otro}` (probá `cargo xtask help`)");
            ExitCode::FAILURE
        }
    }
}

/// Raíz del repo (xtask vive un nivel abajo).
fn raiz() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask siempre tiene padre")
        .to_path_buf()
}

fn correr(titulo: &str, comando: &mut Command) -> bool {
    println!("== {titulo}");
    match comando.status() {
        Ok(estado) if estado.success() => true,
        Ok(estado) => {
            eprintln!("   FALLÓ ({estado})");
            false
        }
        Err(error) => {
            eprintln!("   no se pudo ejecutar: {error}");
            false
        }
    }
}

fn protocol_check() -> ExitCode {
    let raiz = raiz();
    let mut ok = true;

    let mut rust = Command::new("cargo");
    rust.current_dir(&raiz)
        .args(["test", "-p", "ovaultd-protocol", "--test", "v2_conformance"]);
    ok &= correr("conformidad v2 — Rust", &mut rust);

    let mut ts = Command::new("npm");
    ts.current_dir(&raiz)
        .args(["run", "test", "-w", "@ovaultd/protocol"]);
    ok &= correr("conformidad v2 — TypeScript", &mut ts);

    if ok {
        println!("==> protocol-check OK: el fixture v2.json pasa en Rust y TS");
        ExitCode::SUCCESS
    } else {
        eprintln!("==> protocol-check FALLÓ");
        ExitCode::FAILURE
    }
}

fn ayuda() {
    println!(
        "\
xtask — tareas del repo ovaultd

Uso: cargo xtask <tarea>

Tareas:
  protocol-check   Conformidad v2 (Rust + TS) contra tests/fixtures/protocol/v2.json
  backup-restore   Harness de la fase 2: smoke fresco → backup.sh → restore.sh verificado
                   (+ el rechazo de destino no vacío sin --force)
  help             Esta ayuda"
    );
}

/// `backup-restore` — el gate de recuperación operacional de la fase 2.
///
/// Cadena completa y REAL (nada simulado): un smoke fresco crea base + CAS con datos
/// firmados; `backup.sh` archiva con manifiesto y checksums; `restore.sh` restaura a un
/// destino nuevo verificando TODO; y una comparación independiente (`diff -r` + conteos)
/// confirma que lo restaurado es idéntico a lo archivado. Al final, el caso negativo: un
/// destino no vacío SIN `--force` tiene que ser rechazado.
fn backup_restore() -> ExitCode {
    let raiz = raiz();
    let base = raiz.join(".tmp/xtask-backup");

    let _ = std::fs::remove_dir_all(&base);
    if let Err(error) = std::fs::create_dir_all(&base) {
        eprintln!("xtask: no se pudo crear {}: {error}", base.display());
        return ExitCode::FAILURE;
    }

    let mut ok = true;

    // 1. smoke fresco: servidor REAL, base + CAS reales, dos revisiones firmadas, se apaga solo.
    let mut smoke = Command::new("bash");
    smoke.current_dir(&raiz)
        .args(["tests/scripts/server-smoke.sh"])
        .env("SMOKE_PORT", "8793")
        .env("SMOKE_DIR", ".tmp/xtask-backup/smoke");
    ok &= correr("smoke fresco (base + CAS con datos firmados)", &mut smoke);

    // 2. backup (en frío: el smoke ya apagó su server).
    let mut backup = Command::new("bash");
    backup.current_dir(&raiz).args([
        "scripts/backup.sh",
        "--db",
        ".tmp/xtask-backup/smoke/db/metadata.db",
        "--cas",
        ".tmp/xtask-backup/smoke/cas",
        "--output",
        ".tmp/xtask-backup/backup.tar.zst",
    ]);
    ok &= correr("backup.sh (manifiesto + checksums + sidecar sha256)", &mut backup);

    // 3. restore sobre destino nuevo.
    let mut restore = Command::new("bash");
    restore.current_dir(&raiz).args([
        "scripts/restore.sh",
        "--input",
        ".tmp/xtask-backup/backup.tar.zst",
        "--into",
        ".tmp/xtask-backup/restaurado",
        "--project",
        "ovaultd-restore-check",
    ]);
    ok &= correr("restore.sh (checksums + integridad SQL + referencias CAS)", &mut restore);

    // 4. caso negativo: el MISMO destino, SIN --force, tiene que ser rechazado.
    let mut rechazo = Command::new("bash");
    rechazo.current_dir(&raiz).args([
        "scripts/restore.sh",
        "--input",
        ".tmp/xtask-backup/backup.tar.zst",
        "--into",
        ".tmp/xtask-backup/restaurado",
    ]);
    ok &= correr_esperando_fallo("restore.sh rechaza un destino no vacío sin --force", &mut rechazo);

    // 5. caso --force: sobreescribe y termina bien.
    let mut forzado = Command::new("bash");
    forzado.current_dir(&raiz).args([
        "scripts/restore.sh",
        "--input",
        ".tmp/xtask-backup/backup.tar.zst",
        "--into",
        ".tmp/xtask-backup/restaurado",
        "--force",
    ]);
    ok &= correr("restore.sh --force sobre el mismo destino", &mut forzado);

    // 6. verificación independiente: extraer el backup a `control/` y `diff -r` contra lo
    //    restaurado (contenido byte a byte), más las cifras de aceptación de la fase.
    let mut extraer = Command::new("bash");
    extraer
        .current_dir(&raiz)
        .arg("-c")
        .arg("mkdir -p .tmp/xtask-backup/control && zstd -dc .tmp/xtask-backup/backup.tar.zst | tar -C .tmp/xtask-backup/control -xf -");
    ok &= correr("extrayendo el backup a control/ (verificación independiente)", &mut extraer);

    let mut diff = Command::new("diff");
    diff.current_dir(&raiz)
        .args(["-r", ".tmp/xtask-backup/control", ".tmp/xtask-backup/restaurado"]);
    ok &= correr("diff -r control/ restaurado/ (idénticos byte a byte)", &mut diff);

    let verificacion = r#"
import sqlite3, sys
con = sqlite3.connect(sys.argv[1])
def una(sql):
    return con.execute(sql).fetchone()[0]
firmas = una("SELECT count(*) FROM revisions WHERE sig IS NOT NULL")
revisiones = una("SELECT count(*) FROM revisions")
arbol = una("SELECT count(*) FROM files WHERE deleted = 0")
wrappers = una("SELECT count(*) FROM vaults WHERE wrapped_key_pw IS NOT NULL AND wrapped_key_rc IS NOT NULL AND vault_pubkey IS NOT NULL")
assert revisiones >= 2, f"revisiones: {revisiones}"
assert firmas == revisiones, f"firmas {firmas} != revisiones {revisiones}"
assert arbol >= 2, f"files vivos: {arbol}"
assert wrappers >= 1, f"wrappers completos: {wrappers}"
print(f"   aceptación: arbol={arbol} historial={revisiones} (firmas {firmas}) wrappers={wrappers}")
"#;
    let mut aceptacion = Command::new("python3");
    aceptacion
        .current_dir(&raiz)
        .args(["-c", verificacion, ".tmp/xtask-backup/restaurado/metadata.db"]);
    ok &= correr("aceptación: árbol, historial, firmas y wrappers en lo restaurado", &mut aceptacion);

    if ok {
        println!("==> backup-restore OK: backup y restore verificados de punta a punta");
        ExitCode::SUCCESS
    } else {
        eprintln!("==> backup-restore FALLÓ");
        ExitCode::FAILURE
    }
}

/// Corre un comando que DEBE fallar (exit != 0). Un éxito acá es un fallo del harness.
fn correr_esperando_fallo(titulo: &str, comando: &mut Command) -> bool {
    println!("== {titulo}");
    match comando.status() {
        Ok(estado) if !estado.success() => {
            println!("   rechazado como corresponde ({estado})");
            true
        }
        Ok(estado) => {
            eprintln!("   FALLÓ la expectativa: el comando salió {estado} y debía ser rechazado");
            false
        }
        Err(error) => {
            eprintln!("   no se pudo ejecutar: {error}");
            false
        }
    }
}

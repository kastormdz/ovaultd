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
  help             Esta ayuda"
    );
}

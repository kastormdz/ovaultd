//! Conformidad v2 — el MISMO fixture que ejecuta el lado TS
//! (`packages/protocol/test/v2.test.ts`): `tests/fixtures/protocol/v2.json`.
//!
//! El riesgo declarado del plan es "serialización Rust/TS divergente": estos casos
//! la congelan desde los dos lados. El fixture no se regenera: un cambio requiere ADR.

use ovaultd_protocol::{check_content_size, Batch, BlobId, ErrorCode, Op, OperationId, PathId, VaultId};
use serde_json::Value;
use std::fs;

fn fixture() -> Value {
    let ruta = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/protocol/v2.json"
    );
    let texto = fs::read_to_string(ruta).expect("el fixture v2.json debe existir");
    serde_json::from_str(&texto).expect("el fixture v2.json debe ser JSON válido")
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex del fixture"))
        .collect()
}

#[test]
fn op_wire() {
    let fixture = fixture();

    for caso in fixture["op_wire"].as_array().expect("op_wire") {
        if let Some(wire) = caso["wire"].as_str() {
            let op: Op = serde_json::from_value(Value::String(wire.to_owned())).expect("op textual");
            assert_eq!(u8::from(op), caso["code"].as_u64().expect("code") as u8, "op {wire}");
            assert_eq!(
                serde_json::to_value(op).expect("serializa"),
                Value::String(wire.to_owned()),
                "roundtrip de {wire}"
            );
        } else {
            let numero = caso["wire_numeric"].as_u64().expect("wire_numeric");
            let op: Op = serde_json::from_value(Value::from(numero)).expect("op numérica");
            assert_eq!(u8::from(op), caso["code"].as_u64().expect("code") as u8);
        }
    }

    for invalido in fixture["op_wire_invalid"].as_array().expect("op_wire_invalid") {
        assert!(
            serde_json::from_value::<Op>(invalido.clone()).is_err(),
            "op inválida aceptada: {invalido}"
        );
    }
}

#[test]
fn error_codes() {
    let fixture = fixture();

    let esperados: Vec<String> = fixture["error_codes"]
        .as_array()
        .expect("error_codes")
        .iter()
        .map(|valor| valor.as_str().expect("string").to_owned())
        .collect();

    let reales: Vec<String> = ErrorCode::ALL
        .iter()
        .map(|code| code.as_str().to_owned())
        .collect();

    assert_eq!(reales, esperados, "la lista cerrada de ErrorCode cambió");

    for esperado in &esperados {
        assert_eq!(esperado.parse::<ErrorCode>().expect("parsea"), {
            ErrorCode::ALL
                .iter()
                .copied()
                .find(|code| code.as_str() == esperado)
                .expect("existe")
        });
    }
    assert!("nope".parse::<ErrorCode>().is_err());
}

#[test]
fn content_size() {
    let fixture = fixture();

    for caso in fixture["content_size"].as_array().expect("content_size") {
        let size = caso["size"].as_u64().expect("size");
        let ok = caso["ok"].as_bool().expect("ok");
        assert_eq!(check_content_size(size).is_ok(), ok, "size {size}");
    }
}

#[test]
fn operation_id_from_wire() {
    let fixture = fixture();

    for caso in fixture["operation_id_from_wire"]
        .as_array()
        .expect("operation_id_from_wire")
    {
        let wire = caso["wire"].as_str().expect("wire");
        let parseado = OperationId::from_wire(wire);

        if caso.get("invalid").and_then(Value::as_bool).unwrap_or(false) {
            assert!(parseado.is_err(), "debía rechazar: {wire}");
        } else {
            let esperado = unhex(caso["bytes_hex"].as_str().expect("bytes_hex"));
            let operation_id = parseado.expect("debía parsear");
            assert_eq!(operation_id.as_bytes().as_slice(), esperado.as_slice(), "wire {wire}");
            assert_eq!(hex(operation_id.as_bytes()), caso["bytes_hex"].as_str().expect("hex"));
        }
    }
}

#[test]
fn id_b64() {
    let fixture = fixture();

    for caso in fixture["id_b64"].as_array().expect("id_b64") {
        let nombre = caso["name"].as_str().expect("name");
        let bytes = unhex(caso["bytes_hex"].as_str().expect("bytes_hex"));
        let b64 = caso["b64"].as_str().expect("b64");
        let como_json = Value::String(b64.to_owned());

        match nombre {
            "vault_id" => {
                let id: VaultId = serde_json::from_value(como_json).expect("vault_id b64");
                assert_eq!(id.as_bytes().as_slice(), bytes.as_slice());
                assert_eq!(serde_json::to_value(id).expect("serializa"), caso["b64"]);
            }
            "path_id" => {
                let id: PathId = serde_json::from_value(como_json).expect("path_id b64");
                assert_eq!(id.as_bytes().as_slice(), bytes.as_slice());
                assert_eq!(serde_json::to_value(id).expect("serializa"), caso["b64"]);
            }
            "blob_id" => {
                let id: BlobId = serde_json::from_value(como_json).expect("blob_id b64");
                assert_eq!(id.as_bytes().as_slice(), bytes.as_slice());
                assert_eq!(serde_json::to_value(id).expect("serializa"), caso["b64"]);
            }
            "operation_id" => {
                let id: OperationId = serde_json::from_value(como_json).expect("operation_id b64");
                assert_eq!(id.as_bytes().as_slice(), bytes.as_slice());
                assert_eq!(serde_json::to_value(id).expect("serializa"), caso["b64"]);
            }
            otro => panic!("id_b64 con nombre desconocido: {otro}"),
        }
    }
}

#[test]
fn batch_examples() {
    let fixture = fixture();

    for caso in fixture["batch_examples"].as_array().expect("batch_examples") {
        let nombre = caso["name"].as_str().expect("name");
        let batch: Batch = serde_json::from_value(caso["batch"].clone())
            .unwrap_or_else(|error| panic!("el batch {nombre} no deserializa: {error}"));

        let resultado = match batch.validate() {
            Ok(()) => "ok",
            Err(code) => code.as_str(),
        };

        assert_eq!(
            resultado,
            caso["result"].as_str().expect("result"),
            "batch {nombre}"
        );
    }
}

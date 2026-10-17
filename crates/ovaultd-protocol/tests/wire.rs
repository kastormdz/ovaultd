//! Gate del **wire format** de `ovaultd-protocol` contra `docs/protocol.md`.
//!
//! Esto no prueba que el código compile: prueba que los bytes que viajan son los que el documento dice.
//! Un `Batch` con las claves cambiadas, un id en hex en vez de base64 o un código de error fuera de la lista
//! compilan perfecto y rompen la interop con el otro extremo.

use ovaultd_protocol::{
    Batch, BatchAck, BatchOp, BlobId, ErrorCode, MAX_BATCH_OPS, MAX_FILE_BYTES, Op, OperationId,
    PathId, VaultId, check_content_size,
};

/// `docs/protocol.md`: los identificadores y las firmas viajan en **base64**, no en hex.
#[test]
fn los_ids_viajan_en_base64_con_la_longitud_exacta() {
    let vault = VaultId::new([0x01; 16]);
    let path = PathId::new([0x02; 32]);
    let blob = BlobId::new([0x03; 32]);
    let op = OperationId::new([0x04; 16]);

    let jv = serde_json::to_string(&vault).expect("VaultId serializa");
    assert!(
        jv.starts_with('"') && jv.ends_with('"'),
        "tiene que ser string JSON: {jv}"
    );
    assert_eq!(
        jv.trim_matches('"').len(),
        24,
        "16 B en base64 = 24 chars con padding"
    );
    assert_eq!(
        serde_json::to_string(&path)
            .expect("PathId")
            .trim_matches('"')
            .len(),
        44
    );
    assert_eq!(
        serde_json::to_string(&blob)
            .expect("BlobId")
            .trim_matches('"')
            .len(),
        44
    );
    assert_eq!(
        serde_json::to_string(&op)
            .expect("OperationId")
            .trim_matches('"')
            .len(),
        24
    );

    // roundtrip exacto
    let back: VaultId = serde_json::from_str(&jv).expect("VaultId deserializa");
    assert_eq!(back, vault);
}

/// Una longitud equivocada en el wire tiene que **fallar**, no recortar ni rellenar.
#[test]
fn un_id_de_longitud_incorrecta_es_rechazado() {
    let corto = format!("\"{}\"", "AAAA"); // 3 B, no 16
    assert!(serde_json::from_str::<VaultId>(&corto).is_err());
    // y el hex no es base64 valido para rellenar 16 B: tiene que fallar igual
    let hex32 = format!("\"{}\"", "aa".repeat(32));
    assert!(serde_json::from_str::<VaultId>(&hex32).is_err());
}

/// Las claves del `Batch` son las del documento (`docs/protocol.md`, ejemplo del mensaje `batch`).
#[test]
fn las_claves_del_batch_son_las_del_documento() {
    let batch_id = serde_json::to_string(&OperationId::new([0x04; 16]))
        .expect("OperationId serializa")
        .trim_matches('"')
        .to_string();
    let operation_id = batch_id.clone();

    let batch = Batch {
        t: "batch".to_string(),
        batch_id,
        operation_id,
        expected_rev: 1834,
        ops: vec![
            BatchOp {
                op: Op::Mod,
                path_id: PathId::new([0x02; 32]),
                path_enc: Some("cGF0aA==".to_string()),
                folder: false,
                blob_id: Some(BlobId::new([0x03; 32])),
                t_mac: Some("ZmlybWE=".to_string()),
                sig: "ZmlybWE=".to_string(),
                size: Some(12043),
                mtime: Some(1_758_831_000_000),
                ctime: Some(1_758_800_000_000),
                prev_path_id: None,
            },
            BatchOp {
                op: Op::Move,
                path_id: PathId::new([0x02; 32]),
                path_enc: Some("cGF0aA==".to_string()),
                folder: false,
                blob_id: Some(BlobId::new([0x03; 32])),
                t_mac: Some("ZmlybWE=".to_string()),
                sig: "ZmlybWE=".to_string(),
                size: Some(12043),
                mtime: Some(1_758_831_000_000),
                ctime: Some(1_758_800_000_000),
                prev_path_id: Some(PathId::new([0x02; 32])),
            },
            BatchOp {
                op: Op::Del,
                path_id: PathId::new([0x02; 32]),
                path_enc: None,
                folder: false,
                blob_id: None,
                t_mac: None,
                sig: "ZmlybWE=".to_string(),
                size: None,
                mtime: None,
                ctime: None,
                prev_path_id: None,
            },
        ],
    };
    let value = serde_json::to_value(&batch).expect("Batch serializa");
    let obj = value.as_object().expect("Batch es un objeto JSON");

    for clave in ["t", "batch_id", "operation_id", "expected_rev", "ops"] {
        assert!(
            obj.contains_key(clave),
            "falta la clave `{clave}` en el Batch; objeto: {obj:?}"
        );
    }
    assert_eq!(obj["t"], "batch", "t tiene que ser `batch`, fue {}", obj["t"]);
    assert!(
        obj["batch_id"].is_string(),
        "batch_id tiene que ser string base64, fue {}",
        obj["batch_id"]
    );
    assert_eq!(
        obj["batch_id"].as_str().expect("batch_id string").len(),
        24,
        "batch_id tiene que medir 24 chars base64, fue {}",
        obj["batch_id"]
    );
    assert!(
        obj["operation_id"].is_string(),
        "operation_id tiene que ser string base64, fue {}",
        obj["operation_id"]
    );
    assert_eq!(
        obj["operation_id"]
            .as_str()
            .expect("operation_id string")
            .len(),
        24,
        "operation_id tiene que medir 24 chars base64, fue {}",
        obj["operation_id"]
    );
    assert!(
        obj["expected_rev"].is_number(),
        "expected_rev tiene que ser numerico, fue {}",
        obj["expected_rev"]
    );
    assert_eq!(
        obj["ops"].as_array().expect("ops array").len(),
        3,
        "ops tiene que contener 3 elementos, fue {}",
        obj["ops"]
    );

    for (indice, op) in obj["ops"]
        .as_array()
        .expect("ops array")
        .iter()
        .enumerate()
    {
        for clave in [
            "op",
            "path_id",
            "path_enc",
            "folder",
            "blob_id",
            "t_mac",
            "sig",
            "size",
            "mtime",
            "ctime",
            "prev_path_id",
        ] {
            assert!(
                op.as_object()
                    .expect("cada op es un objeto")
                    .contains_key(clave),
                "falta la clave `{clave}` en la op {indice}; op: {op}"
            );
        }
    }

    let mod_op = &obj["ops"][0];
    assert_eq!(
        mod_op["op"], "mod",
        "la op mod tiene que viajar como string `mod`, fue {}",
        mod_op["op"]
    );
    assert!(
        mod_op["path_enc"].is_string(),
        "path_enc de mod tiene que ser string, fue {}",
        mod_op["path_enc"]
    );
    assert!(
        mod_op["folder"].is_boolean(),
        "folder de mod tiene que ser booleano, fue {}",
        mod_op["folder"]
    );
    assert!(
        mod_op["blob_id"].is_string(),
        "blob_id de mod tiene que ser string, fue {}",
        mod_op["blob_id"]
    );
    assert_eq!(
        mod_op["prev_path_id"],
        serde_json::Value::Null,
        "prev_path_id de mod tiene que ser null, fue {}",
        mod_op["prev_path_id"]
    );

    let move_op = &obj["ops"][1];
    assert!(
        move_op["prev_path_id"].is_string(),
        "prev_path_id de move tiene que ser string, fue {}",
        move_op["prev_path_id"]
    );

    let del_op = &obj["ops"][2];
    assert_eq!(
        del_op["path_enc"],
        serde_json::Value::Null,
        "path_enc de del tiene que ser null, fue {}",
        del_op["path_enc"]
    );
    assert_eq!(
        del_op["blob_id"],
        serde_json::Value::Null,
        "blob_id de del tiene que ser null, fue {}",
        del_op["blob_id"]
    );
    assert_eq!(
        del_op["t_mac"],
        serde_json::Value::Null,
        "t_mac de del tiene que ser null, fue {}",
        del_op["t_mac"]
    );
    assert_eq!(
        del_op["size"],
        serde_json::Value::Null,
        "size de del tiene que ser null, fue {}",
        del_op["size"]
    );
    assert!(
        del_op["sig"].is_string(),
        "sig de del tiene que ser string porque el tombstone va firmado, fue {}",
        del_op["sig"]
    );
}

/// Las claves del `BatchAck` son las del documento (`docs/protocol.md`, ejemplo del mensaje `batch_ack`).
#[test]
fn las_claves_del_batch_ack_son_las_del_documento() {
    let ack = BatchAck {
        t: "batch_ack".to_string(),
        batch_id: serde_json::to_string(&OperationId::new([0x04; 16]))
            .expect("OperationId serializa")
            .trim_matches('"')
            .to_string(),
        rev: 1835,
    };
    let value = serde_json::to_value(&ack).expect("BatchAck serializa");
    let obj = value.as_object().expect("BatchAck es un objeto JSON");

    for clave in ["t", "batch_id", "rev"] {
        assert!(
            obj.contains_key(clave),
            "falta la clave `{clave}` en el BatchAck; objeto: {obj:?}"
        );
    }
    assert_eq!(
        obj["t"], "batch_ack",
        "t tiene que ser `batch_ack`, fue {}",
        obj["t"]
    );
    assert!(
        obj["rev"].is_number(),
        "rev tiene que ser numerico, fue {}",
        obj["rev"]
    );
}

/// Un batch sin operaciones no representa una revision valida.
#[test]
fn un_batch_vacio_es_rechazado() {
    let batch = Batch {
        t: "batch".to_string(),
        batch_id: "BAQEBAQEBAQEBAQEBAQEBA==".to_string(),
        operation_id: "BAQEBAQEBAQEBAQEBAQEBA==".to_string(),
        expected_rev: 1834,
        ops: Vec::new(),
    };

    assert_eq!(
        batch.validate(),
        Err(ErrorCode::BadRequest),
        "un batch con ops vacio tiene que devolver BadRequest, fue {:?}",
        batch.validate()
    );
}

/// El tope de operaciones de ADR-0008 se aplica en el limite exacto.
#[test]
fn el_tope_de_ops_se_aplica_en_el_limite() {
    fn carpeta_op(indice: u8) -> BatchOp {
        BatchOp {
            op: Op::Mod,
            path_id: PathId::new([indice; 32]),
            path_enc: Some("cGF0aA==".to_string()),
            folder: true,
            blob_id: None,
            t_mac: None,
            sig: "ZmlybWE=".to_string(),
            size: None,
            mtime: Some(1_758_831_000_000),
            ctime: Some(1_758_800_000_000),
            prev_path_id: None,
        }
    }

    let mut ops = Vec::new();
    for indice in 0..MAX_BATCH_OPS {
        ops.push(carpeta_op((indice % 256) as u8));
    }

    let batch = Batch {
        t: "batch".to_string(),
        batch_id: "BAQEBAQEBAQEBAQEBAQEBA==".to_string(),
        operation_id: "BAQEBAQEBAQEBAQEBAQEBA==".to_string(),
        expected_rev: 1834,
        ops,
    };
    assert_eq!(
        batch.validate(),
        Ok(()),
        "un batch con exactamente MAX_BATCH_OPS={} ops tiene que ser valido, fue {:?}",
        MAX_BATCH_OPS,
        batch.validate()
    );

    let mut ops = batch.ops;
    ops.push(carpeta_op(0));
    let batch_excedido = Batch { ops, ..batch };

    assert_eq!(
        batch_excedido.validate(),
        Err(ErrorCode::BadRequest),
        "un batch con MAX_BATCH_OPS+1={} ops tiene que devolver BadRequest, fue {:?}",
        MAX_BATCH_OPS + 1,
        batch_excedido.validate()
    );
}

/// Una operación `del` es un tombstone y no puede referenciar un blob.
#[test]
fn una_del_no_lleva_blob_ni_nombre() {
    let op = BatchOp {
        op: Op::Del,
        path_id: PathId::new([0x02; 32]),
        path_enc: None,
        folder: false,
        blob_id: Some(BlobId::new([0x03; 32])),
        t_mac: None,
        sig: "ZmlybWE=".to_string(),
        size: None,
        mtime: None,
        ctime: None,
        prev_path_id: None,
    };

    assert_eq!(
        op.validate(),
        Err(ErrorCode::BadRequest),
        "una del con blob_id={:?} tiene que devolver BadRequest, fue {:?}",
        op.blob_id,
        op.validate()
    );
}

/// Una carpeta no puede llevar contenido.
#[test]
fn una_carpeta_no_lleva_blob() {
    let op = BatchOp {
        op: Op::Mod,
        path_id: PathId::new([0x02; 32]),
        path_enc: Some("cGF0aA==".to_string()),
        folder: true,
        blob_id: Some(BlobId::new([0x03; 32])),
        t_mac: Some("ZmlybWE=".to_string()),
        sig: "ZmlybWE=".to_string(),
        size: None,
        mtime: Some(1_758_831_000_000),
        ctime: Some(1_758_800_000_000),
        prev_path_id: None,
    };

    assert_eq!(
        op.validate(),
        Err(ErrorCode::BadRequest),
        "una carpeta con blob_id={:?} tiene que devolver BadRequest, fue {:?}",
        op.blob_id,
        op.validate()
    );
}

/// El tope de contenido de ADR-0007 se aplica en el limite exacto.
#[test]
fn el_tope_de_contenido_se_aplica_en_el_limite() {
    assert_eq!(MAX_FILE_BYTES, 33_554_432, "32 MiB segun ADR-0007");
    assert!(check_content_size(0).is_ok());
    assert!(
        check_content_size(MAX_FILE_BYTES).is_ok(),
        "el tope exacto es valido"
    );
    assert_eq!(
        check_content_size(MAX_FILE_BYTES + 1),
        Err(ErrorCode::MaxFileExceeded)
    );
}

/// La lista de codigos de error es **cerrada**: cada uno va y vuelve por su string exacto,
/// y ninguno lleva el prefijo `t:` (eso es de los mensajes).
#[test]
fn la_lista_de_errores_es_cerrada_y_sin_prefijo() {
    assert_eq!(ErrorCode::ALL.len(), 7);
    for code in ErrorCode::ALL {
        let texto = code.as_str();
        assert!(
            !texto.starts_with("t:"),
            "el codigo {texto} no lleva prefijo t:"
        );
        assert!(
            !texto.contains(':'),
            "el codigo {texto} tiene que ser snake_case pelado"
        );
        assert_eq!(texto.parse::<ErrorCode>().expect("roundtrip"), code);
        let json = serde_json::to_string(&code).expect("serializa");
        assert_eq!(json, format!("\"{texto}\""));
    }
    assert!(
        "conflict".parse::<ErrorCode>().is_err(),
        "conflict es un MENSAJE, no un error"
    );
    assert!("max_file_exceeded".parse::<ErrorCode>().is_ok());
}

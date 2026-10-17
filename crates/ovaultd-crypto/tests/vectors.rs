//! Gate de los **32 vectores dorados** de `sync2` (`tests/fixtures/crypto/sync2.json`).
//!
//! Por qué existe este archivo y no un test adentro de `lib.rs`: porque el test que había antes era
//! **vacuo** —verificaba que el fixture fuese JSON válido y nada más— y pasó un test de mutación sin
//! fallar. Un test verde que no verifica nada es peor que no tener test: compra confianza falsa.
//!
//! ## Qué fija cada grupo
//!
//! - **Deterministas** (se reproducen byte a byte): `KEK`, las seis subclaves, `PUBKEY`, `PATH_ID`,
//!   `NONCE_BODY`, `BLOB`, `BLOB_ID`, `T_MAC`, `SIG`.
//! - **Nonce explícito**: `WRAPPED_PW`, `WRAPPED_RC` y `PATH_ENC` se guardan *bare* (`ciphertext || tag`)
//!   con el nonce fijado por el vector, así que se **descifran** contra el valor congelado en vez de
//!   intentar reproducirlos (el nonce de producción es aleatorio y vive dentro del blob). El `BLOB`
//!   de contenido, desde ADR-0014, se emite en su **forma almacenada** (`nonce ‖ ciphertext ‖ tag`):
//!   el lector saca el nonce del prefijo — sin eso, quien no tenga el plaintext no puede descifrar.
//! - **Negativos y propiedades**: los cinco casos negativos tienen que dar `Err` (nunca `panic`), el
//!   dedupe tiene que dar `SAME`/`DIFFERENT` y un rename **no** puede cambiar el `blob_id`.
//!
//! Las entradas del vector son las del spike de referencia (`spikes/crypto-interop/rust/src/main.rs`).

use serde_json::Value;
use ovaultd_crypto::{
    KdfParams, VaultIdentity, VaultKey, blob_id, decrypt_blob, derive_kek, derive_kek_recovery,
    derive_subkeys, encrypt_blob, msg_rec, nonce_body, path_dec_with_nonce, path_id, sign_record,
    t_mac, unwrap_pw_with_nonce, unwrap_rc_with_nonce, verify_record,
};

const FIXTURE: &str = include_str!("../../../tests/fixtures/crypto/sync2.json");

// --- entradas del vector (idénticas al spike) ---
const PASSWORD: &str = "contrase\u{00f1}a-\u{0065}\u{0301}-\u{6771}\u{4eac}";
const PW_NFKC: &str = "contrase\u{00f1}a-\u{00e9}-\u{6771}\u{4eac}";
const SALT_HEX: &str = "000102030405060708090a0b0c0d0e0f";
const VAULT_ID_HEX: &str = "0102030405060708090a0b0c0d0e0f10";
const OPERATION_ID_HEX: &str = "00112233445566778899aabbccddeeff";
// OJO: el string de PATH es ENTRADA del vector dorado (sync2.json) y del spike: la nota del
// vault se renombró a ovaultd-plan.md, pero acá queda congelado a propósito — cambiarlo
// invalida los vectores ya generados.
const PATH: &str = "02-hermes-docs/hermes-core/obsidian-sync-plugin-plan.md";
const RENAMED_PATH: &str = "renombrado.md";
const PLAINTEXT: &[u8] = b"# nota de prueba\nsin acentos rotos: \xc3\xb1\n";
const OTHER_PLAINTEXT: &[u8] = b"# nota diferente\n";
const OP_MOD: u8 = 0x02;
const EPOCH: u16 = 1;
const SIZE: u64 = 39;
const MTIME: u64 = 1_758_831_000_000;
const EXPECTED_REV: u64 = 1834;
const NONCE_PW: [u8; 24] = [0x01; 24];
const NONCE_RC: [u8; 24] = [0x02; 24];
const NONCE_PATH: [u8; 24] = [0x03; 24];

fn hx(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn unhex(text: &str, key: &str) -> Vec<u8> {
    assert!(
        text.len() % 2 == 0,
        "el vector {key} tiene longitud hex impar"
    );
    (0..text.len() / 2)
        .map(|i| {
            u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)
                .unwrap_or_else(|_| panic!("vector {key} no es hex"))
        })
        .collect()
}

fn fx() -> Value {
    serde_json::from_str(FIXTURE).expect("el fixture tiene que ser JSON")
}

fn esperado(fixture: &Value, key: &str) -> Vec<u8> {
    let texto = fixture
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("falta el vector `{key}` en el fixture"));
    unhex(texto, key)
}

fn pin(obtenido: &[u8], fixture: &Value, key: &str) {
    let want = esperado(fixture, key);
    assert_eq!(
        hx(obtenido),
        hx(&want),
        "\nvector `{key}` NO coincide.\n  esperado: {}\n  obtenido: {}\n",
        hx(&want),
        hx(obtenido)
    );
}

fn pin_etiqueta(fixture: &Value, key: &str, want: &str) {
    let got = fixture
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("falta el vector `{key}`"));
    assert_eq!(got, want, "la etiqueta del vector {key} cambio");
}

#[test]
fn los_32_vectores_dorados_se_verifican() {
    let fixture = fx();

    let salt: [u8; 16] = unhex(SALT_HEX, "salt").try_into().expect("salt de 16 B");
    let vault_id: [u8; 16] = unhex(VAULT_ID_HEX, "vault_id")
        .try_into()
        .expect("vault_id de 16 B");
    let operation_id: [u8; 16] = unhex(OPERATION_ID_HEX, "operation_id")
        .try_into()
        .expect("operation_id de 16 B");
    let vault_key_bytes: [u8; 32] = core::array::from_fn(|i| i as u8);
    let recovery_secret: [u8; 32] = core::array::from_fn(|i| 0x20u8 + i as u8);

    // ---------------------------------------------------------------- derivación
    pin_etiqueta(&fixture, "PW_NFKC", PW_NFKC);

    let kek = derive_kek(PASSWORD, &salt, &KdfParams::sync2_defaults()).expect("argon2id");
    pin(&kek, &fixture, "KEK");

    let kek_recovery = derive_kek_recovery(&recovery_secret, &salt);
    assert!(
        fixture.get("KEK_RECOVERY").is_none(),
        "KEK_RECOVERY no esta en el fixture: no se inventa"
    );

    pin(&vault_key_bytes, &fixture, "VAULT_KEY");

    let sub = derive_subkeys(&VaultKey(vault_key_bytes), &salt);
    pin(&sub.content, &fixture, "K_CONTENT");
    pin(&sub.nonce, &fixture, "K_NONCE");
    pin(&sub.path, &fixture, "K_PATH");
    pin(&sub.meta, &fixture, "K_META");
    pin(&sub.bind, &fixture, "K_BIND");
    pin(&sub.sign, &fixture, "K_SIGN");

    let identity = VaultIdentity::generate(&sub.sign);
    pin(&identity.pubkey(), &fixture, "PUBKEY");

    // ---------------------------------------------------------------- envoltura (nonce explícito)
    let wrapped_pw = esperado(&fixture, "WRAPPED_PW");
    let devuelto = unwrap_pw_with_nonce(&kek, &vault_id, &salt, &NONCE_PW, &wrapped_pw)
        .expect("WRAPPED_PW tiene que desenvolver con el KEK del vector");
    assert_eq!(
        hx(devuelto.as_bytes()),
        hx(&vault_key_bytes),
        "WRAPPED_PW no devuelve la vault_key del vector"
    );
    pin_etiqueta(&fixture, "UNWRAP", "OK");

    let wrapped_rc = esperado(&fixture, "WRAPPED_RC");
    let devuelto_rc = unwrap_rc_with_nonce(&kek_recovery, &vault_id, &NONCE_RC, &wrapped_rc)
        .expect("WRAPPED_RC tiene que desenvolver con la KEK de recovery");
    assert_eq!(hx(devuelto_rc.as_bytes()), hx(&vault_key_bytes));

    // ---------------------------------------------------------------- paths
    let id = path_id(&sub, PATH);
    pin(&id, &fixture, "PATH_ID");

    let path_enc = esperado(&fixture, "PATH_ENC");
    let recuperado = path_dec_with_nonce(&sub, &id, &NONCE_PATH, &path_enc)
        .expect("PATH_ENC tiene que descifrar con su path_id");
    assert_eq!(recuperado, PATH, "PATH_ENC no devuelve el path del vector");
    pin_etiqueta(&fixture, "PATH_ENC_RT", "OK");

    // ---------------------------------------------------------------- contenido
    let nonce = nonce_body(&sub, PLAINTEXT);
    pin(&nonce, &fixture, "NONCE_BODY");

    let blob = encrypt_blob(&sub, PLAINTEXT).expect("cifra el contenido");
    pin(&blob, &fixture, "BLOB");
    assert_eq!(
        fixture.get("BLOB_LEN").and_then(Value::as_str),
        Some(blob.len().to_string().as_str()),
        "BLOB_LEN no coincide con el blob obtenido"
    );

    let devuelto = decrypt_blob(&sub, &blob).expect("descifra el contenido (nonce del prefijo)");
    assert_eq!(
        devuelto, PLAINTEXT,
        "BLOB no devuelve el plaintext del vector"
    );
    pin_etiqueta(&fixture, "BLOB_RT", "OK");

    let bid = blob_id(&blob);
    pin(&bid, &fixture, "BLOB_ID");

    // ---------------------------------------------------------------- binding y firma
    let mac = t_mac(&identity, &vault_id, &id, &bid, EPOCH);
    pin(&mac, &fixture, "T_MAC");

    let firma = sign_record(
        &identity,
        &id,
        &bid,
        &mac,
        OP_MOD,
        SIZE,
        MTIME,
        EXPECTED_REV,
        &operation_id,
        EPOCH,
    );
    pin(&firma, &fixture, "SIG");

    let public_key = identity.pubkey();
    verify_record(
        &public_key,
        &id,
        &bid,
        &mac,
        OP_MOD,
        SIZE,
        MTIME,
        EXPECTED_REV,
        &operation_id,
        EPOCH,
        &firma,
    )
    .expect("SIG tiene que verificar con la publica del vector");
    pin_etiqueta(&fixture, "SIG_VERIFY", "OK");
    assert_eq!(
        msg_rec(
            &id,
            &bid,
            &mac,
            OP_MOD,
            SIZE,
            MTIME,
            EXPECTED_REV,
            &operation_id,
            EPOCH
        )
        .len(),
        9 + 1 + 32 + 32 + 64 + 24 + 16 + 2, // dominio + op + path_id + blob_id + t_mac + size/mtime/rev + operation_id + epoch
        "los bytes firmados cambiaron de tamano"
    );

    // ---------------------------------------------------------------- negativos
    let wrong_kek = derive_kek(&format!("{PASSWORD}x"), &salt, &KdfParams::sync2_defaults())
        .expect("argon2id con password incorrecta");
    assert!(
        unwrap_pw_with_nonce(&wrong_kek, &vault_id, &salt, &NONCE_PW, &wrapped_pw).is_err(),
        "NEG_WRONG_PW: una password incorrecta NO puede desenvolver"
    );
    pin_etiqueta(&fixture, "NEG_WRONG_PW", "REJECTED");

    let otro_vault: [u8; 16] = {
        let mut v = vault_id;
        v[0] ^= 0x01;
        v
    };
    assert!(
        unwrap_pw_with_nonce(&kek, &otro_vault, &salt, &NONCE_PW, &wrapped_pw).is_err(),
        "NEG_AAD: cambiar el vault_id (que entra en el AAD) NO puede desenvolver"
    );
    pin_etiqueta(&fixture, "NEG_AAD", "REJECTED");

    let otro_id = path_id(&sub, RENAMED_PATH);
    assert!(
        path_dec_with_nonce(&sub, &otro_id, &NONCE_PATH, &path_enc).is_err(),
        "NEG_SWAP_PATH: descifrar con otro path_id (AAD) NO puede funcionar"
    );
    pin_etiqueta(&fixture, "NEG_SWAP_PATH", "REJECTED");

    let mut firma_tocada = firma;
    firma_tocada[0] ^= 0x01;
    assert!(
        verify_record(
            &public_key,
            &id,
            &bid,
            &mac,
            OP_MOD,
            SIZE,
            MTIME,
            EXPECTED_REV,
            &operation_id,
            EPOCH,
            &firma_tocada,
        )
        .is_err(),
        "NEG_SIG: una firma alterada NO puede verificar"
    );
    pin_etiqueta(&fixture, "NEG_SIG", "REJECTED");

    let truncado = &blob[..blob.len() - 1];
    assert!(
        decrypt_blob(&sub, truncado).is_err(),
        "NEG_TRUNC: un ciphertext truncado NO puede descifrar"
    );
    pin_etiqueta(&fixture, "NEG_TRUNC", "REJECTED");

    // ---------------------------------------------------------------- propiedades
    let blob_otra_vez = encrypt_blob(&sub, PLAINTEXT).expect("cifra el mismo contenido otra vez");
    let id_otra_vez = blob_id(&blob_otra_vez);
    assert_eq!(
        hx(&id_otra_vez),
        hx(&bid),
        "DEDUPE_SAME: el mismo contenido tiene que dar el mismo blob_id"
    );
    assert_eq!(
        hx(&blob_otra_vez),
        hx(&blob),
        "el mismo contenido tiene que dar el mismo blob"
    );
    pin_etiqueta(&fixture, "DEDUPE_SAME", "SAME");

    let blob_otro = encrypt_blob(&sub, OTHER_PLAINTEXT).expect("cifra contenido distinto");
    let id_otro = blob_id(&blob_otro);
    assert_ne!(
        hx(&id_otro),
        hx(&bid),
        "DEDUPE_DIFF: contenido distinto tiene que dar blob_id distinto"
    );
    pin_etiqueta(&fixture, "DEDUPE_DIFF", "DIFFERENT");

    assert_ne!(
        hx(&otro_id),
        hx(&id),
        "RENAME: el path_id de un rename tiene que ser distinto"
    );
    let blob_del_renombrado = encrypt_blob(&sub, PLAINTEXT).expect("cifra tras el rename");
    assert_eq!(
        hx(&blob_id(&blob_del_renombrado)),
        hx(&bid),
        "RENAME: el blob_id NO puede cambiar al renombrar (el contenido es el mismo)"
    );
    pin_etiqueta(&fixture, "RENAME", "OK");
}

fn datos_de_firma() -> (
    VaultIdentity,
    [u8; 32],
    [u8; 32],
    [u8; 64],
    [u8; 16],
) {
    let salt: [u8; 16] = unhex(SALT_HEX, "salt").try_into().expect("salt de 16 B");
    let vault_id: [u8; 16] = unhex(VAULT_ID_HEX, "vault_id")
        .try_into()
        .expect("vault_id de 16 B");
    let operation_id: [u8; 16] = unhex(OPERATION_ID_HEX, "operation_id")
        .try_into()
        .expect("operation_id de 16 B");
    let vault_key_bytes: [u8; 32] = core::array::from_fn(|i| i as u8);
    let sub = derive_subkeys(&VaultKey(vault_key_bytes), &salt);
    let identity = VaultIdentity::generate(&sub.sign);
    let path_id = path_id(&sub, PATH);
    let blob = encrypt_blob(&sub, PLAINTEXT).expect("cifra el contenido");
    let blob_id = blob_id(&blob);
    let t_mac = t_mac(&identity, &vault_id, &path_id, &blob_id, EPOCH);

    (identity, path_id, blob_id, t_mac, operation_id)
}

#[test]
fn msg_rec_tiene_180_bytes_y_la_op_en_el_byte_9() {
    let (identity, path_id, blob_id, t_mac, operation_id) = datos_de_firma();

    for op in [1u8, 2, 3, 4] {
        let message = msg_rec(
            &path_id,
            &blob_id,
            &t_mac,
            op,
            SIZE,
            MTIME,
            EXPECTED_REV,
            &operation_id,
            EPOCH,
        );

        assert_eq!(message.len(), 180);
        assert_eq!(message[9], op);
        let _ = &identity;
    }
}

#[test]
fn dos_ops_distintas_dan_firmas_distintas() {
    let (identity, path_id, blob_id, t_mac, operation_id) = datos_de_firma();

    let firma_mod = sign_record(
        &identity,
        &path_id,
        &blob_id,
        &t_mac,
        2,
        SIZE,
        MTIME,
        EXPECTED_REV,
        &operation_id,
        EPOCH,
    );
    let firma_del = sign_record(
        &identity,
        &path_id,
        &blob_id,
        &t_mac,
        3,
        SIZE,
        MTIME,
        EXPECTED_REV,
        &operation_id,
        EPOCH,
    );

    assert_ne!(firma_mod, firma_del);
}

#[test]
fn dos_expected_rev_distintos_dan_firmas_distintas() {
    let (identity, path_id, blob_id, t_mac, operation_id) = datos_de_firma();

    let firma_1834 = sign_record(
        &identity,
        &path_id,
        &blob_id,
        &t_mac,
        OP_MOD,
        SIZE,
        MTIME,
        1834,
        &operation_id,
        EPOCH,
    );
    let firma_1835 = sign_record(
        &identity,
        &path_id,
        &blob_id,
        &t_mac,
        OP_MOD,
        SIZE,
        MTIME,
        1835,
        &operation_id,
        EPOCH,
    );

    assert_ne!(firma_1834, firma_1835);
}

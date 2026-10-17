use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::XChaCha20Poly1305;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

const DOMAIN: &str = "sync2";

fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut out = [0u8; 32];
    hk.expand(info, &mut out).expect("hkdf expand");
    out
}

fn derive_kek(password: &[u8], salt: &[u8]) -> [u8; 32] {
    let params = Params::new(65536, 3, 4, Some(32)).expect("argon2 params");
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    argon2
        .hash_password_into(password, salt, &mut out)
        .expect("argon2");
    out
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("hmac key");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

fn main() {
    let password = "contrase\u{00f1}a-\u{0065}\u{0301}-\u{6771}\u{4eac}";
    let pw_nfkc: String = password.nfkc().collect();
    let salt: [u8; 16] = hex::decode("000102030405060708090a0b0c0d0e0f")
        .expect("salt")
        .try_into()
        .expect("salt length");
    let vault_id: [u8; 16] = hex::decode("0102030405060708090a0b0c0d0e0f10")
        .expect("vault id")
        .try_into()
        .expect("vault id length");
    let vault_key: [u8; 32] = core::array::from_fn(|i| i as u8);
    let rec_secret: [u8; 32] = core::array::from_fn(|i| 0x20u8 + i as u8);
    let operation_id: [u8; 16] = hex::decode("00112233445566778899aabbccddeeff")
        .expect("operation id")
        .try_into()
        .expect("operation id length");

    let kek = derive_kek(pw_nfkc.as_bytes(), &salt);
    let kek_recovery = hkdf32(&rec_secret, &salt, b"sync2/kek-recovery");

    let mut aad_pw = b"sync2/vaultkey/pw".to_vec();
    aad_pw.extend_from_slice(&vault_id);
    aad_pw.extend_from_slice(&salt);
    aad_pw.extend_from_slice(&65536u32.to_be_bytes());
    aad_pw.extend_from_slice(&3u32.to_be_bytes());
    aad_pw.extend_from_slice(&4u32.to_be_bytes());
    aad_pw.extend_from_slice(&32u32.to_be_bytes());

    let mut aad_rc = b"sync2/vaultkey/rc".to_vec();
    aad_rc.extend_from_slice(&vault_id);

    let cipher_pw = XChaCha20Poly1305::new((&kek).into());
    let cipher_rc = XChaCha20Poly1305::new((&kek_recovery).into());
    let nonce_pw = [0x01u8; 24];
    let nonce_rc = [0x02u8; 24];

    let wrapped_pw = cipher_pw
        .encrypt(
            (&nonce_pw).into(),
            Payload {
                msg: &vault_key,
                aad: &aad_pw,
            },
        )
        .expect("encrypt wrapped password");
    let wrapped_rc = cipher_rc
        .encrypt(
            (&nonce_rc).into(),
            Payload {
                msg: &vault_key,
                aad: &aad_rc,
            },
        )
        .expect("encrypt wrapped recovery");

    let unwrap_pw = cipher_pw.decrypt(
        (&nonce_pw).into(),
        Payload {
            msg: &wrapped_pw,
            aad: &aad_pw,
        },
    );
    let unwrap_rc = cipher_rc.decrypt(
        (&nonce_rc).into(),
        Payload {
            msg: &wrapped_rc,
            aad: &aad_rc,
        },
    );
    let unwrap_ok = matches!(unwrap_pw, Ok(ref value) if value == &vault_key.to_vec())
        && matches!(unwrap_rc, Ok(ref value) if value == &vault_key.to_vec());

    let k_content = hkdf32(&vault_key, &salt, format!("{}/content", DOMAIN).as_bytes());
    let k_nonce = hkdf32(&vault_key, &salt, format!("{}/nonce", DOMAIN).as_bytes());
    let k_path = hkdf32(&vault_key, &salt, format!("{}/path", DOMAIN).as_bytes());
    let k_meta = hkdf32(&vault_key, &salt, format!("{}/meta", DOMAIN).as_bytes());
    let k_bind = hkdf32(&vault_key, &salt, format!("{}/bind", DOMAIN).as_bytes());
    let k_sign = hkdf32(&vault_key, &salt, format!("{}/sign", DOMAIN).as_bytes());

    let signing_key = SigningKey::from_bytes(&k_sign);
    let public_key = signing_key.verifying_key().to_bytes();

    // OJO: string congelado a propósito — es entrada del vector dorado; la nota del vault se
    // renombró a ovaultd-plan.md (cambiarlo acá rompe la comparación con sync2.json).
    let path = "02-hermes-docs/hermes-core/obsidian-sync-plugin-plan.md";
    let path_nfkc: String = path.nfkc().collect();
    let path_id = hmac_sha256(&k_path, path_nfkc.as_bytes());

    let cipher_path = XChaCha20Poly1305::new((&k_path).into());
    let nonce_path = [0x03u8; 24];
    let path_enc = cipher_path
        .encrypt(
            (&nonce_path).into(),
            Payload {
                msg: path_nfkc.as_bytes(),
                aad: &path_id,
            },
        )
        .expect("encrypt path");
    let path_enc_rt = cipher_path
        .decrypt(
            (&nonce_path).into(),
            Payload {
                msg: &path_enc,
                aad: &path_id,
            },
        )
        .map(|value| value == path_nfkc.as_bytes())
        .unwrap_or(false);

    let plaintext = b"# nota de prueba\nsin acentos rotos: \xc3\xb1\n";
    let nonce_full = hmac_sha256(&k_nonce, plaintext);
    let nonce_body: [u8; 24] = nonce_full[..24].try_into().expect("nonce length");

    let cipher_content = XChaCha20Poly1305::new((&k_content).into());
    // ADR-0014: el blob almacenado es `nonce[24] ‖ ciphertext ‖ tag[16]` — el nonce viaja con el
    // contenido (el lector no puede derivarlo: no tiene el plaintext).
    let mut blob = nonce_body.to_vec();
    blob.extend_from_slice(
        &cipher_content
            .encrypt(
                (&nonce_body).into(),
                Payload {
                    msg: plaintext,
                    aad: b"",
                },
            )
            .expect("encrypt blob"),
    );
    let stored_nonce: [u8; 24] = blob[..24].try_into().expect("nonce length");
    let blob_rt = cipher_content
        .decrypt(
            (&stored_nonce).into(),
            Payload {
                msg: &blob[24..],
                aad: b"",
            },
        )
        .map(|value| value == plaintext)
        .unwrap_or(false);
    let blob_id: [u8; 32] = Sha256::digest(&blob).into();

    let mut msg_bind = b"sync2/bind".to_vec();
    msg_bind.extend_from_slice(&vault_id);
    msg_bind.extend_from_slice(&path_id);
    msg_bind.extend_from_slice(&blob_id);
    msg_bind.extend_from_slice(&1u16.to_be_bytes());
    let t_mac = signing_key.sign(&msg_bind).to_bytes();

    let mut msg_rec = b"sync2/rec".to_vec();
    msg_rec.push(0x02);
    msg_rec.extend_from_slice(&path_id);
    msg_rec.extend_from_slice(&blob_id);
    msg_rec.extend_from_slice(&t_mac);
    msg_rec.extend_from_slice(&39u64.to_be_bytes());
    msg_rec.extend_from_slice(&1758831000000u64.to_be_bytes());
    msg_rec.extend_from_slice(&1834u64.to_be_bytes());
    msg_rec.extend_from_slice(&operation_id);
    msg_rec.extend_from_slice(&1u16.to_be_bytes());
    let sig = signing_key.sign(&msg_rec).to_bytes();
    let sig_verify = signing_key
        .verifying_key()
        .verify(&msg_rec, &Signature::from_bytes(&sig))
        .is_ok();

    let mut wrong_password = pw_nfkc.as_bytes().to_vec();
    wrong_password.push(b'x');
    let wrong_kek = derive_kek(&wrong_password, &salt);
    let wrong_cipher = XChaCha20Poly1305::new((&wrong_kek).into());
    let neg_wrong_pw = wrong_cipher
        .decrypt(
            (&nonce_pw).into(),
            Payload {
                msg: &wrapped_pw,
                aad: &aad_pw,
            },
        )
        .map(|value| value == vault_key.to_vec())
        .unwrap_or(false);

    let mut altered_aad = aad_pw.clone();
    let last = altered_aad.len() - 1;
    altered_aad[last] ^= 1;
    let neg_aad = cipher_pw
        .decrypt(
            (&nonce_pw).into(),
            Payload {
                msg: &wrapped_pw,
                aad: &altered_aad,
            },
        )
        .map(|value| value == vault_key.to_vec())
        .unwrap_or(false);

    let other_path = "otro.md";
    let other_path_id = hmac_sha256(&k_path, other_path.as_bytes());
    let other_path_enc = cipher_path
        .encrypt(
            (&nonce_path).into(),
            Payload {
                msg: other_path.as_bytes(),
                aad: &other_path_id,
            },
        )
        .expect("encrypt other path");
    let neg_swap_path = cipher_path
        .decrypt(
            (&nonce_path).into(),
            Payload {
                msg: &other_path_enc,
                aad: &path_id,
            },
        )
        .map(|value| value == other_path.as_bytes())
        .unwrap_or(false);

    let mut altered_sig = sig;
    altered_sig[63] ^= 1;
    let neg_sig = signing_key
        .verifying_key()
        .verify(&msg_rec, &Signature::from_bytes(&altered_sig))
        .is_ok();

    let truncated_blob = &blob[..blob.len() - 1];
    let neg_trunc = cipher_content
        .decrypt(
            (&stored_nonce).into(),
            Payload {
                msg: &truncated_blob[24..],
                aad: b"",
            },
        )
        .map(|value| value == plaintext)
        .unwrap_or(false);

    let mut blob_again = nonce_body.to_vec();
    blob_again.extend_from_slice(
        &cipher_content
            .encrypt(
                (&nonce_body).into(),
                Payload {
                    msg: plaintext,
                    aad: b"",
                },
            )
            .expect("encrypt duplicate blob"),
    );
    let blob_again_id: [u8; 32] = Sha256::digest(&blob_again).into();

    let other_plaintext = b"# nota diferente\n";
    let other_nonce_full = hmac_sha256(&k_nonce, other_plaintext);
    let other_nonce: [u8; 24] = other_nonce_full[..24].try_into().expect("nonce length");
    let mut other_blob = other_nonce.to_vec();
    other_blob.extend_from_slice(
        &cipher_content
            .encrypt(
                (&other_nonce).into(),
                Payload {
                    msg: other_plaintext,
                    aad: b"",
                },
            )
            .expect("encrypt different blob"),
    );
    let other_blob_id: [u8; 32] = Sha256::digest(&other_blob).into();

    let renamed_path = "renombrado.md";
    let renamed_path_id = hmac_sha256(&k_path, renamed_path.as_bytes());

    println!("PW_NFKC\t{}", pw_nfkc);
    println!("KEK\t{}", hex::encode(kek));
    println!("VAULT_KEY\t{}", hex::encode(vault_key));
    println!("WRAPPED_PW\t{}", hex::encode(wrapped_pw));
    println!("WRAPPED_RC\t{}", hex::encode(wrapped_rc));
    println!("UNWRAP\t{}", if unwrap_ok { "OK" } else { "FAIL" });
    println!("K_CONTENT\t{}", hex::encode(k_content));
    println!("K_NONCE\t{}", hex::encode(k_nonce));
    println!("K_PATH\t{}", hex::encode(k_path));
    println!("K_META\t{}", hex::encode(k_meta));
    println!("K_BIND\t{}", hex::encode(k_bind));
    println!("K_SIGN\t{}", hex::encode(k_sign));
    println!("PUBKEY\t{}", hex::encode(public_key));
    println!("PATH_ID\t{}", hex::encode(path_id));
    println!("PATH_ENC\t{}", hex::encode(path_enc));
    println!("PATH_ENC_RT\t{}", if path_enc_rt { "OK" } else { "FAIL" });
    println!("NONCE_BODY\t{}", hex::encode(nonce_body));
    println!("BLOB\t{}", hex::encode(&blob));
    println!("BLOB_LEN\t{}", blob.len());
    println!("BLOB_ID\t{}", hex::encode(blob_id));
    println!("BLOB_RT\t{}", if blob_rt { "OK" } else { "FAIL" });
    println!("T_MAC\t{}", hex::encode(t_mac));
    println!("SIG\t{}", hex::encode(sig));
    println!("SIG_VERIFY\t{}", if sig_verify { "OK" } else { "FAIL" });
    println!(
        "NEG_WRONG_PW\t{}",
        if neg_wrong_pw { "LEAKED" } else { "REJECTED" }
    );
    println!(
        "NEG_AAD\t{}",
        if neg_aad { "LEAKED" } else { "REJECTED" }
    );
    println!(
        "NEG_SWAP_PATH\t{}",
        if neg_swap_path { "LEAKED" } else { "REJECTED" }
    );
    println!("NEG_SIG\t{}", if neg_sig { "OK" } else { "REJECTED" });
    println!(
        "NEG_TRUNC\t{}",
        if neg_trunc { "LEAKED" } else { "REJECTED" }
    );
    println!(
        "DEDUPE_SAME\t{}",
        if blob_id == blob_again_id {
            "SAME"
        } else {
            "DIFFERENT"
        }
    );
    println!(
        "DEDUPE_DIFF\t{}",
        if blob_id == other_blob_id {
            "SAME"
        } else {
            "DIFFERENT"
        }
    );
    println!(
        "RENAME\t{}",
        if blob_id[..] == Sha256::digest(&blob)[..] && path_id != renamed_path_id {
            "OK"
        } else {
            "FAIL"
        }
    );
}

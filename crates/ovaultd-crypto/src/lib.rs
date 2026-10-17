use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    XChaCha20Poly1305,
    aead::{Aead, KeyInit, Payload},
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::{error::Error, fmt, fs::File, io::Read};
use unicode_normalization::UnicodeNormalization;

const MAX_CONTENT_SIZE: u64 = 32 * 1024 * 1024;
const WRAPPED_NONCE_LEN: usize = 24;
const AEAD_TAG_LEN: usize = 16;
const ED25519_SIGNATURE_LEN: usize = 64;

/// Sobre de un blob almacenado (también de envolturas y `path_enc`): `nonce[24] + tag[16]`
/// (ADR-0014). El tope de subida del cuerpo cifrado es `MAX_FILE_BYTES + BLOB_ENVELOPE_BYTES`.
pub const BLOB_ENVELOPE_BYTES: u64 = (WRAPPED_NONCE_LEN + AEAD_TAG_LEN) as u64;

type HmacSha256 = Hmac<Sha256>;

/// Resultado usado por este crate.
pub type Result<T> = std::result::Result<T, CryptoError>;

/// Errores criptográficos y de validación de `sync2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    /// Los parámetros de KDF no son válidos.
    InvalidKdfParams,
    /// La entrada no tiene la longitud requerida.
    InvalidLength,
    /// La contraseña no pudo desbloquear la clave del vault.
    WrongPassword,
    /// La autenticación AEAD falló.
    AuthenticationFailed,
    /// La firma no es válida.
    InvalidSignature,
    /// La entrada excede el tamaño permitido.
    ContentTooLarge,
    /// La fuente de aleatoriedad del sistema falló.
    RandomnessUnavailable,
    /// La operación solicitada no pudo completarse.
    InvalidInput(&'static str),
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKdfParams => write!(f, "invalid KDF parameters"),
            Self::InvalidLength => write!(f, "invalid input length"),
            Self::WrongPassword => write!(f, "incorrect password"),
            Self::AuthenticationFailed => write!(f, "AEAD authentication failed"),
            Self::InvalidSignature => write!(f, "invalid signature"),
            Self::ContentTooLarge => write!(f, "content exceeds the 32 MiB limit"),
            Self::RandomnessUnavailable => write!(f, "system randomness unavailable"),
            Self::InvalidInput(message) => write!(f, "invalid input: {message}"),
        }
    }
}

impl Error for CryptoError {}

/// Parámetros versionados de Argon2id usados por la sección de derivación de KEK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    /// Memoria en KiB.
    pub m_kib: u32,
    /// Número de iteraciones.
    pub t: u32,
    /// Paralelismo.
    pub p: u32,
    /// Longitud de salida.
    pub dk_len: u32,
}

impl KdfParams {
    /// Devuelve los parámetros Argon2id oficiales de `sync2`.
    pub const fn sync2_defaults() -> Self {
        Self {
            m_kib: 65_536,
            t: 3,
            p: 4,
            dk_len: 32,
        }
    }
}

/// Deriva la KEK desde una contraseña normalizada NFKC, según la sección
/// “Arquitectura de claves — password, KEK y clave de datos”.
pub fn derive_kek(password: &str, salt: &[u8; 16], p: &KdfParams) -> Result<[u8; 32]> {
    if p.dk_len != 32 || p.m_kib == 0 || p.t == 0 || p.p == 0 {
        return Err(CryptoError::InvalidKdfParams);
    }

    let normalized: String = password.nfkc().collect();
    let params =
        Params::new(p.m_kib, p.t, p.p, Some(32)).map_err(|_| CryptoError::InvalidKdfParams)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut output = [0u8; 32];

    argon2
        .hash_password_into(normalized.as_bytes(), salt, &mut output)
        .map_err(|_| CryptoError::InvalidKdfParams)?;

    Ok(output)
}

/// Clave aleatoria de datos del vault.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VaultKey(pub [u8; 32]);

impl fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("VaultKey").field(&"[redacted]").finish()
    }
}

impl VaultKey {
    /// Genera una nueva clave aleatoria de vault.
    pub fn generate() -> Result<Self> {
        let mut key = [0u8; 32];
        fill_random(&mut key)?;
        Ok(Self(key))
    }

    /// Devuelve la clave como bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Envuelve la clave con la KEK de password.
    ///
    /// El resultado tiene el layout `nonce[24] || ciphertext || tag`.
    pub fn wrap_pw(&self, kek: &[u8; 32], vault_id: &[u8; 16], salt: &[u8; 16]) -> Result<Vec<u8>> {
        let nonce = random_nonce()?;
        let aad = vaultkey_pw_aad(vault_id, salt, &KdfParams::sync2_defaults());
        let cipher = XChaCha20Poly1305::new(kek.into());
        let ciphertext = cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: &self.0,
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::AuthenticationFailed)?;

        let mut result = Vec::with_capacity(WRAPPED_NONCE_LEN + ciphertext.len());
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    /// Desenvuelve una clave protegida por password.
    pub fn unwrap_pw(
        kek: &[u8; 32],
        vault_id: &[u8; 16],
        salt: &[u8; 16],
        wrapped: &[u8],
    ) -> Result<Self> {
        unwrap_key(
            kek,
            &vaultkey_pw_aad(vault_id, salt, &KdfParams::sync2_defaults()),
            wrapped,
        )
        .map_err(|_| CryptoError::WrongPassword)
    }

    /// Envuelve la clave con una KEK de recovery.
    ///
    /// El resultado tiene el layout `nonce[24] || ciphertext || tag`.
    pub fn wrap_rc(&self, kek_recovery: &[u8; 32], vault_id: &[u8; 16]) -> Result<Vec<u8>> {
        let nonce = random_nonce()?;
        let aad = vaultkey_rc_aad(vault_id);
        let cipher = XChaCha20Poly1305::new(kek_recovery.into());
        let ciphertext = cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: &self.0,
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::AuthenticationFailed)?;

        let mut result = Vec::with_capacity(WRAPPED_NONCE_LEN + ciphertext.len());
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    /// Desenvuelve una clave protegida por recovery.
    pub fn unwrap_rc(&self, kek_recovery: &[u8; 32], vault_id: &[u8; 16], wrapped: &[u8]) -> Result<Self> {
        unwrap_key(kek_recovery, &vaultkey_rc_aad(vault_id), wrapped)
    }
}

/// Subclaves derivadas mediante HKDF-SHA-256.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubKeys {
    /// Clave de cifrado de blobs.
    pub content: [u8; 32],
    /// Clave de derivación de nonces de blobs.
    pub nonce: [u8; 32],
    /// Clave de paths.
    pub path: [u8; 32],
    /// Clave de metadata.
    pub meta: [u8; 32],
    /// Clave de binding auxiliar.
    pub bind: [u8; 32],
    /// Seed de identidad Ed25519.
    pub sign: [u8; 32],
}

/// Deriva las seis subclaves de la sección “Arquitectura de claves”.
pub fn derive_subkeys(vault_key: &VaultKey, salt: &[u8; 16]) -> SubKeys {
    SubKeys {
        content: hkdf32(&vault_key.0, salt, b"sync2/content"),
        nonce: hkdf32(&vault_key.0, salt, b"sync2/nonce"),
        path: hkdf32(&vault_key.0, salt, b"sync2/path"),
        meta: hkdf32(&vault_key.0, salt, b"sync2/meta"),
        bind: hkdf32(&vault_key.0, salt, b"sync2/bind"),
        sign: hkdf32(&vault_key.0, salt, b"sync2/sign"),
    }
}

/// Identidad Ed25519 determinista del vault.
#[derive(Clone)]
pub struct VaultIdentity {
    signing: SigningKey,
}

impl VaultIdentity {
    /// Construye una identidad determinista desde `k_sign`.
    pub fn generate(k_sign: &[u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(k_sign),
        }
    }

    /// Devuelve la clave pública Ed25519.
    pub fn pubkey(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// Firma un mensaje.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing.sign(message).to_bytes()
    }

    /// Verifica una firma contra una clave pública.
    pub fn verify(public_key: &[u8; 32], message: &[u8], signature: &[u8]) -> Result<()> {
        if signature.len() != ED25519_SIGNATURE_LEN {
            return Err(CryptoError::InvalidLength);
        }

        let verifying_key =
            VerifyingKey::from_bytes(public_key).map_err(|_| CryptoError::InvalidSignature)?;
        let signature =
            Signature::from_slice(signature).map_err(|_| CryptoError::InvalidSignature)?;

        verifying_key
            .verify(message, &signature)
            .map_err(|_| CryptoError::InvalidSignature)
    }
}

/// Calcula `path_id = HMAC-SHA256(k_path, NFKC(path))`.
pub fn path_id(keys: &SubKeys, path: &str) -> [u8; 32] {
    let normalized: String = path.nfkc().collect();
    hmac_sha256(&keys.path, normalized.as_bytes())
}

/// Cifra un path usando un nonce aleatorio y `path_id` como AAD.
///
/// El resultado tiene el layout `nonce[24] || ciphertext || tag`.
pub fn path_enc(keys: &SubKeys, path: &str) -> Result<Vec<u8>> {
    let normalized: String = path.nfkc().collect();
    let id = hmac_sha256(&keys.path, normalized.as_bytes());
    let nonce = random_nonce()?;
    let cipher = XChaCha20Poly1305::new((&keys.path).into());
    let ciphertext = cipher
        .encrypt(
            (&nonce).into(),
            Payload {
                msg: normalized.as_bytes(),
                aad: &id,
            },
        )
        .map_err(|_| CryptoError::AuthenticationFailed)?;

    let mut result = Vec::with_capacity(WRAPPED_NONCE_LEN + ciphertext.len());
    result.extend_from_slice(&nonce);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

/// Descifra un path usando `path_id` como AAD.
pub fn path_dec(keys: &SubKeys, path_id: &[u8; 32], encoded: &[u8]) -> Result<String> {
    if encoded.len() < WRAPPED_NONCE_LEN + AEAD_TAG_LEN {
        return Err(CryptoError::InvalidLength);
    }

    let nonce: [u8; 24] = encoded[..24]
        .try_into()
        .map_err(|_| CryptoError::InvalidLength)?;
    let cipher = XChaCha20Poly1305::new((&keys.path).into());
    let plaintext = cipher
        .decrypt(
            (&nonce).into(),
            Payload {
                msg: &encoded[24..],
                aad: path_id,
            },
        )
        .map_err(|_| CryptoError::AuthenticationFailed)?;

    String::from_utf8(plaintext).map_err(|_| CryptoError::InvalidInput("path is not UTF-8"))
}

/// Deriva el nonce MLE keyed de un contenido.
pub fn nonce_body(keys: &SubKeys, plaintext: &[u8]) -> [u8; 24] {
    let digest = hmac_sha256(&keys.nonce, plaintext);
    digest[..24].try_into().expect("fixed nonce length")
}

/// Cifra un blob: el resultado es `nonce[24] ‖ ciphertext ‖ tag[16]` (ADR-0014).
///
/// El nonce sigue siendo `nonce_body` (determinista por plaintext: el dedupe no cambia), pero
/// ahora **viaja con el blob**: sin él, un lector que no tenga ya el plaintext no puede
/// descifrar — el "derivable" de ADR-0009 valía solo para el escritor.
pub fn encrypt_blob(keys: &SubKeys, plaintext: &[u8]) -> Result<Vec<u8>> {
    let nonce = nonce_body(keys, plaintext);
    let cipher = XChaCha20Poly1305::new((&keys.content).into());

    let ciphertext = cipher
        .encrypt(
            (&nonce).into(),
            Payload {
                msg: plaintext,
                aad: b"",
            },
        )
        .map_err(|_| CryptoError::AuthenticationFailed)?;

    let mut result = Vec::with_capacity(WRAPPED_NONCE_LEN + ciphertext.len());
    result.extend_from_slice(&nonce);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

/// Descifra un blob **almacenado** (`nonce[24] ‖ ciphertext ‖ tag[16]`): el nonce sale del
/// prefijo, no del plaintext — este es el camino del lector (ADR-0014).
pub fn decrypt_blob(keys: &SubKeys, blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < WRAPPED_NONCE_LEN + AEAD_TAG_LEN {
        return Err(CryptoError::InvalidLength);
    }

    let mut nonce = [0u8; WRAPPED_NONCE_LEN];
    nonce.copy_from_slice(&blob[..WRAPPED_NONCE_LEN]);

    decrypt_blob_with_nonce(keys, &nonce, &blob[WRAPPED_NONCE_LEN..])
}

/// Descifra un blob **bare** (`ciphertext ‖ tag`, sin nonce) con el nonce explícito: lo usan los
/// vectores dorados (que guardan el nonce fijado por el test) y el formato histórico.
pub fn decrypt_blob_with_nonce(keys: &SubKeys, nonce: &[u8; 24], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < AEAD_TAG_LEN {
        return Err(CryptoError::InvalidLength);
    }

    let cipher = XChaCha20Poly1305::new((&keys.content).into());
    cipher
        .decrypt(
            nonce.into(),
            Payload {
                msg: blob,
                aad: b"",
            },
        )
        .map_err(|_| CryptoError::AuthenticationFailed)
}

/// Calcula `blob_id = SHA-256(ciphertext)`.
pub fn blob_id(blob: &[u8]) -> [u8; 32] {
    Sha256::digest(blob).into()
}

/// Construye los bytes canónicos de `T`.
pub fn t_mac(
    identity: &VaultIdentity,
    vault_id: &[u8; 16],
    path_id: &[u8; 32],
    blob_id: &[u8; 32],
    epoch: u16,
) -> [u8; 64] {
    let mut message = Vec::with_capacity(8 + 16 + 32 + 32 + 2);
    message.extend_from_slice(b"sync2/bind");
    message.extend_from_slice(vault_id);
    message.extend_from_slice(path_id);
    message.extend_from_slice(blob_id);
    message.extend_from_slice(&epoch.to_be_bytes());
    identity.sign(&message)
}

/// Construye los bytes canónicos de un registro del oplog.
///
/// `"sync2/rec" ‖ u8(op) ‖ path_id(32) ‖ blob_id(32) ‖ t_mac(64) ‖ u64be(size) ‖ u64be(mtime) ‖ u64be(expected_rev) ‖ operation_id(16) ‖ u16be(epoch)`
///
/// El tercer `u64` es `expected_rev`, la base firmada por el cliente que
/// permite re-verificar la firma después del push (ADR-0006); el byte inicial
/// es la operación, no el `enc_version` del vault.
pub fn msg_rec(
    path_id: &[u8; 32],
    blob_id: &[u8; 32],
    t: &[u8; 64],
    op: u8,
    size: u64,
    mtime: u64,
    expected_rev: u64,
    operation_id: &[u8; 16],
    epoch: u16,
) -> Vec<u8> {
    let mut message = Vec::with_capacity(1 + 32 + 32 + 64 + 1 + 8 * 3 + 16 + 2);
    message.extend_from_slice(b"sync2/rec");
    message.push(op);
    message.extend_from_slice(path_id);
    message.extend_from_slice(blob_id);
    message.extend_from_slice(t);
    message.extend_from_slice(&size.to_be_bytes());
    message.extend_from_slice(&mtime.to_be_bytes());
    message.extend_from_slice(&expected_rev.to_be_bytes());
    message.extend_from_slice(operation_id);
    message.extend_from_slice(&epoch.to_be_bytes());
    message
}

/// Firma un registro del oplog.
pub fn sign_record(
    identity: &VaultIdentity,
    path_id: &[u8; 32],
    blob_id: &[u8; 32],
    t: &[u8; 64],
    op: u8,
    size: u64,
    mtime: u64,
    expected_rev: u64,
    operation_id: &[u8; 16],
    epoch: u16,
) -> [u8; 64] {
    let message = msg_rec(
        path_id,
        blob_id,
        t,
        op,
        size,
        mtime,
        expected_rev,
        operation_id,
        epoch,
    );
    identity.sign(&message)
}

/// Verifica la firma de un registro del oplog.
pub fn verify_record(
    public_key: &[u8; 32],
    path_id: &[u8; 32],
    blob_id: &[u8; 32],
    t: &[u8; 64],
    op: u8,
    size: u64,
    mtime: u64,
    expected_rev: u64,
    operation_id: &[u8; 16],
    epoch: u16,
    signature: &[u8],
) -> Result<()> {
    let message = msg_rec(
        path_id,
        blob_id,
        t,
        op,
        size,
        mtime,
        expected_rev,
        operation_id,
        epoch,
    );
    VaultIdentity::verify(public_key, &message, signature)
}

/// Construye el mensaje canónico de una prueba de posesión.
///
/// El layout es `challenge ‖ u64be(vault_id) ‖ u64be(device_id)`.
pub fn proof_msg(vault_id: i64, device_id: i64, challenge: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(challenge.len() + 16);
    message.extend_from_slice(challenge);
    message.extend_from_slice(&vault_id.to_be_bytes());
    message.extend_from_slice(&device_id.to_be_bytes());
    message
}

/// Firma una prueba de posesión con la subclave de firma del vault.
pub fn sign_proof(
    keys: &SubKeys,
    vault_id: i64,
    device_id: i64,
    challenge: &[u8],
) -> [u8; 64] {
    let message = proof_msg(vault_id, device_id, challenge);
    VaultIdentity::generate(&keys.sign).sign(&message)
}

/// Verifica una prueba de posesión contra la clave pública del vault.
pub fn verify_proof(
    vault_pubkey: &[u8; 32],
    vault_id: i64,
    device_id: i64,
    challenge: &[u8],
    proof: &[u8; 64],
) -> Result<()> {
    let message = proof_msg(vault_id, device_id, challenge);
    VaultIdentity::verify(vault_pubkey, &message, proof)
}

/// Comprueba el techo de contenido de 32 MiB definido por `sync2`.
pub fn check_content_size(size: u64) -> Result<()> {
    if size > MAX_CONTENT_SIZE {
        Err(CryptoError::ContentTooLarge)
    } else {
        Ok(())
    }
}

fn hkdf32(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut output = [0u8; 32];
    hkdf.expand(info, &mut output)
        .expect("HKDF output length is fixed and valid");
    output
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut mac = <HmacSha256 as hmac::digest::KeyInit>::new_from_slice(key)
        .expect("HMAC accepts keys of arbitrary nonzero length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

fn vaultkey_pw_aad(vault_id: &[u8; 16], salt: &[u8; 16], params: &KdfParams) -> Vec<u8> {
    let mut aad = Vec::with_capacity(18 + 16 + 16 + 16);
    aad.extend_from_slice(b"sync2/vaultkey/pw");
    aad.extend_from_slice(vault_id);
    aad.extend_from_slice(salt); // ata la envoltura a los parametros de KDF (lo exige el vector)
    aad.extend_from_slice(&params.m_kib.to_be_bytes());
    aad.extend_from_slice(&params.t.to_be_bytes());
    aad.extend_from_slice(&params.p.to_be_bytes());
    aad.extend_from_slice(&params.dk_len.to_be_bytes());
    aad
}

fn vaultkey_rc_aad(vault_id: &[u8; 16]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(16 + 16);
    aad.extend_from_slice(b"sync2/vaultkey/rc");
    aad.extend_from_slice(vault_id);
    aad
}

fn unwrap_key(key: &[u8; 32], aad: &[u8], wrapped: &[u8]) -> Result<VaultKey> {
    if wrapped.len() < WRAPPED_NONCE_LEN + AEAD_TAG_LEN {
        return Err(CryptoError::InvalidLength);
    }

    let nonce: [u8; 24] = wrapped[..24]
        .try_into()
        .map_err(|_| CryptoError::InvalidLength)?;
    let cipher = XChaCha20Poly1305::new(key.into());
    let plaintext = cipher
        .decrypt(
            (&nonce).into(),
            Payload {
                msg: &wrapped[24..],
                aad,
            },
        )
        .map_err(|_| CryptoError::AuthenticationFailed)?;

    if plaintext.len() != 32 {
        return Err(CryptoError::InvalidLength);
    }

    Ok(VaultKey(
        plaintext
            .try_into()
            .map_err(|_| CryptoError::InvalidLength)?,
    ))
}

fn random_nonce() -> Result<[u8; 24]> {
    let mut nonce = [0u8; 24];
    fill_random(&mut nonce)?;
    Ok(nonce)
}

fn fill_random(buffer: &mut [u8]) -> Result<()> {
    let mut file = File::open("/dev/urandom").map_err(|_| CryptoError::RandomnessUnavailable)?;
    file.read_exact(buffer)
        .map_err(|_| CryptoError::RandomnessUnavailable)
}

// ---------------------------------------------------------------------------
// Variantes con **nonce explícito**: existen para poder verificar los vectores
// dorados congelados, que fijan el nonce del test y guardan solo `ciphertext || tag`
// (formato "bare"). En producción se usa la variante sin nonce, que lo lleva dentro
// del blob almacenado.
// ---------------------------------------------------------------------------

/// Desenvuelve `wrapped_key_pw` desde un `nonce` explícito y el `ciphertext` pelado.
pub fn unwrap_pw_with_nonce(
    kek: &[u8; 32],
    vault_id: &[u8; 16],
    salt: &[u8; 16],
    nonce: &[u8; 24],
    ciphertext: &[u8],
) -> Result<VaultKey> {
    let aad = vaultkey_pw_aad(vault_id, salt, &KdfParams::sync2_defaults());
    let cipher = XChaCha20Poly1305::new(kek.into());
    let plaintext = cipher
        .decrypt(
            nonce.into(),
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::WrongPassword)?;

    let bytes: [u8; 32] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::InvalidLength)?;
    Ok(VaultKey(bytes))
}

/// Desenvuelve `wrapped_key_rc` desde un `nonce` explícito y el `ciphertext` pelado.
pub fn unwrap_rc_with_nonce(
    kek_recovery: &[u8; 32],
    vault_id: &[u8; 16],
    nonce: &[u8; 24],
    ciphertext: &[u8],
) -> Result<VaultKey> {
    let aad = vaultkey_rc_aad(vault_id);
    let cipher = XChaCha20Poly1305::new(kek_recovery.into());
    let plaintext = cipher
        .decrypt(
            nonce.into(),
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| CryptoError::AuthenticationFailed)?;

    let bytes: [u8; 32] = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::InvalidLength)?;
    Ok(VaultKey(bytes))
}

/// Descifra un `path_enc` **bare** (`ciphertext || tag`) con un `nonce` explícito.
pub fn path_dec_with_nonce(
    keys: &SubKeys,
    path_id: &[u8; 32],
    nonce: &[u8; 24],
    ciphertext: &[u8],
) -> Result<String> {
    let cipher = XChaCha20Poly1305::new((&keys.path).into());
    let plaintext = cipher
        .decrypt(
            nonce.into(),
            Payload {
                msg: ciphertext,
                aad: path_id,
            },
        )
        .map_err(|_| CryptoError::AuthenticationFailed)?;

    String::from_utf8(plaintext).map_err(|_| CryptoError::InvalidInput("path is not UTF-8"))
}

/// Deriva la KEK de recovery desde el secreto de recovery y el salt.
pub fn derive_kek_recovery(recovery_secret: &[u8; 32], salt: &[u8; 16]) -> [u8; 32] {
    hkdf32(recovery_secret, salt, b"sync2/kek-recovery")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_keys(seed: u8) -> SubKeys {
        let vault_key = VaultKey([seed; 32]);
        derive_subkeys(&vault_key, &[seed.wrapping_add(1); 16])
    }

    #[test]
    fn la_firma_de_prueba_verifica_con_su_publica() {
        let keys = test_keys(1);
        let identity = VaultIdentity::generate(&keys.sign);
        let challenge = b"challenge";

        let proof = sign_proof(&keys, 7, 9, challenge);

        assert_eq!(
            verify_proof(&identity.pubkey(), 7, 9, challenge, &proof),
            Ok(())
        );
    }

    #[test]
    fn el_mismo_challenge_con_otro_vault_id_no_verifica() {
        let keys = test_keys(1);
        let identity = VaultIdentity::generate(&keys.sign);
        let challenge = b"challenge";

        let proof = sign_proof(&keys, 7, 9, challenge);

        assert!(verify_proof(&identity.pubkey(), 8, 9, challenge, &proof).is_err());
    }

    #[test]
    fn el_mismo_challenge_con_otro_device_id_no_verifica() {
        let keys = test_keys(1);
        let identity = VaultIdentity::generate(&keys.sign);
        let challenge = b"challenge";

        let proof = sign_proof(&keys, 7, 9, challenge);

        assert!(verify_proof(&identity.pubkey(), 7, 10, challenge, &proof).is_err());
    }

    #[test]
    fn otro_challenge_no_verifica() {
        let keys = test_keys(1);
        let identity = VaultIdentity::generate(&keys.sign);

        let proof = sign_proof(&keys, 7, 9, b"challenge");

        assert!(verify_proof(&identity.pubkey(), 7, 9, b"otro challenge", &proof).is_err());
    }

    #[test]
    fn una_firma_de_otra_clave_no_verifica() {
        let first_keys = test_keys(1);
        let other_keys = test_keys(2);
        let first_identity = VaultIdentity::generate(&first_keys.sign);
        let challenge = b"challenge";

        let proof = sign_proof(&other_keys, 7, 9, challenge);

        assert!(verify_proof(
            &first_identity.pubkey(),
            7,
            9,
            challenge,
            &proof
        )
        .is_err());
    }

    #[test]
    fn el_layout_es_el_documentado() {
        let message = proof_msg(7, 9, &[0u8; 24]);

        assert_eq!(message.len(), 40);
        assert_eq!(
            &message[24..],
            &[
                7u64.to_be_bytes().as_slice(),
                9u64.to_be_bytes().as_slice(),
            ]
            .concat()
        );
    }
}

/**
 * `CryptoProvider` — la superficie criptográfica que `sync2` necesita.
 *
 * Todo el paquete pasa por acá: cambiar de implementación (noble hoy; WASM/WebCrypto
 * mañana para el móvil) no toca una línea del esquema. La implementación por defecto
 * (`nobleCryptoProvider`) usa @noble con las MISMAS versiones que el spike de interop.
 */

import { xchacha20poly1305 } from '@noble/ciphers/chacha.js';
import { ed25519 } from '@noble/curves/ed25519.js';
import { argon2id } from '@noble/hashes/argon2.js';
import { hkdf } from '@noble/hashes/hkdf.js';
import { hmac } from '@noble/hashes/hmac.js';
import { sha256 } from '@noble/hashes/sha2.js';

export interface Argon2Params {
  m_kib: number;
  t: number;
  p: number;
  dk_len: number;
}

export interface CryptoProvider {
  randomBytes(length: number): Uint8Array;
  sha256(data: Uint8Array): Uint8Array;
  hmacSha256(key: Uint8Array, data: Uint8Array): Uint8Array;
  hkdfSha256(ikm: Uint8Array, salt: Uint8Array, info: Uint8Array, length: number): Uint8Array;
  argon2id(password: Uint8Array, salt: Uint8Array, params: Argon2Params): Uint8Array;
  xchachaSeal(key: Uint8Array, nonce: Uint8Array, aad: Uint8Array, message: Uint8Array): Uint8Array;
  xchachaOpen(key: Uint8Array, nonce: Uint8Array, aad: Uint8Array, ciphertext: Uint8Array): Uint8Array;
  ed25519PublicKey(seed: Uint8Array): Uint8Array;
  ed25519Sign(seed: Uint8Array, message: Uint8Array): Uint8Array;
  ed25519Verify(publicKey: Uint8Array, message: Uint8Array, signature: Uint8Array): boolean;
}

/**
 * Implementación por defecto: @noble, versiones fijadas a las del spike verificado
 * (`spikes/crypto-interop/js`). El azar sale de WebCrypto (`crypto.getRandomValues`),
 * disponible tanto en Node ≥ 19 como en el navegador.
 */
export const nobleCryptoProvider: CryptoProvider = {
  randomBytes(length: number): Uint8Array {
    const out = new Uint8Array(length);
    globalThis.crypto.getRandomValues(out);
    return out;
  },

  sha256(data: Uint8Array): Uint8Array {
    return sha256(data);
  },

  hmacSha256(key: Uint8Array, data: Uint8Array): Uint8Array {
    return hmac(sha256, key, data);
  },

  hkdfSha256(ikm: Uint8Array, salt: Uint8Array, info: Uint8Array, length: number): Uint8Array {
    return hkdf(sha256, ikm, salt, info, length);
  },

  argon2id(password: Uint8Array, salt: Uint8Array, params: Argon2Params): Uint8Array {
    return argon2id(password, salt, {
      m: params.m_kib,
      t: params.t,
      p: params.p,
      dkLen: params.dk_len,
    });
  },

  xchachaSeal(key: Uint8Array, nonce: Uint8Array, aad: Uint8Array, message: Uint8Array): Uint8Array {
    return xchacha20poly1305(key, nonce, aad).encrypt(message);
  },

  xchachaOpen(
    key: Uint8Array,
    nonce: Uint8Array,
    aad: Uint8Array,
    ciphertext: Uint8Array,
  ): Uint8Array {
    return xchacha20poly1305(key, nonce, aad).decrypt(ciphertext);
  },

  ed25519PublicKey(seed: Uint8Array): Uint8Array {
    return ed25519.getPublicKey(seed);
  },

  ed25519Sign(seed: Uint8Array, message: Uint8Array): Uint8Array {
    return ed25519.sign(message, seed);
  },

  ed25519Verify(publicKey: Uint8Array, message: Uint8Array, signature: Uint8Array): boolean {
    return ed25519.verify(signature, message, publicKey);
  },
};

let activo: CryptoProvider = nobleCryptoProvider;

/** El proveedor activo (noble por defecto). */
export const cryptoProvider = (): CryptoProvider => activo;

/**
 * Reemplaza el proveedor activo (WASM/móvil, o un doble de test).
 * El esquema `sync2` no cambia: solo cambia quién ejecuta las primitivas.
 */
export const setCryptoProvider = (provider: CryptoProvider): void => {
  activo = provider;
};

/**
 * Vectores dorados `sync2` — el MISMO fixture que verifica Rust
 * (`crates/ovaultd-crypto/tests/vectors.rs`): `tests/fixtures/crypto/sync2.json`.
 *
 * Si esto pasa, el port TS cifra, firma y deriva byte a byte igual que el server.
 * El fixture NO se regenera desde acá: es un artefacto congelado (ADR-0014).
 */

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

import { cryptoProvider } from '../src/CryptoProvider.js';
import {
  bytesToUuid,
  decryptBlob,
  decryptPathEnc,
  decryptPathEncWithNonce,
  deriveKek,
  deriveKekRecovery,
  deriveSubkeys,
  encryptBlob,
  blobId,
  identityFromSubkeys,
  nonceBody,
  pathEnc,
  pathId,
  signRecord,
  tMac,
  unwrapKeyPw,
  unwrapKeyRc,
  unwrapPwWithNonce,
  unwrapRcWithNonce,
  vaultkeyRcAad,
  verifyRecord,
  wrapKeyPw,
  wrapKeyRc,
} from '../src/sync2.js';

const fixture = JSON.parse(
  readFileSync(new URL('../../../tests/fixtures/crypto/sync2.json', import.meta.url), 'utf8'),
) as Record<string, string>;

const fromHex = (text: string): Uint8Array =>
  Uint8Array.from(text.match(/../g)!.map((byte) => Number.parseInt(byte, 16)));

const toHex = (bytes: Uint8Array): string =>
  Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');

// ---------------------------------------------------------------------------
// Entradas fijas del vector (las mismas del spike y de vectors.rs)
// ---------------------------------------------------------------------------

const password = 'contraseña-e\u0301-東京';
const salt = fromHex('000102030405060708090a0b0c0d0e0f');
const vaultUid = fromHex('0102030405060708090a0b0c0d0e0f10');
const vaultKey = Uint8Array.from({ length: 32 }, (_, i) => i);
const recoverySecret = Uint8Array.from({ length: 32 }, (_, i) => i + 0x20);
const path = '02-hermes-docs/hermes-core/obsidian-sync-plugin-plan.md';
const plaintext = new TextEncoder().encode('# nota de prueba\nsin acentos rotos: ñ\n');
const epoch = 1;
const opCode = 2; // mod — el vector firma un mod
const size = 39;
const mtime = 1758831000000n;
const expectedRev = 1834n;
const operationId = fromHex('00112233445566778899aabbccddeeff');

const nonceWrappedPw = new Uint8Array(24).fill(0x01);
const nonceWrappedRc = new Uint8Array(24).fill(0x02);
const noncePath = new Uint8Array(24).fill(0x03);

// KDF del vector: los DEFAULTS de sync2 (m = 64 MiB) — el fixture se congeló con ellos.
// deriveKek valida params y normaliza NFKC igual que el Rust.
const deriveKekVector = (pw: string): Uint8Array => deriveKek(pw, salt);

describe('sync2 — vectores dorados', () => {
  it('deriva la KEK (Argon2id, password NFKC)', () => {
    const kek = deriveKekVector(password);
    expect(toHex(kek)).toBe(fixture.KEK);
    expect(password.normalize('NFKC')).toBe(fixture.PW_NFKC);
  }, 60_000);

  it('desenvuelve la vault_key con password y recovery (nonce explícito del vector)', () => {
    const kek = deriveKekVector(password);
    const kekRecovery = deriveKekRecovery(recoverySecret, salt);

    const viaPw = unwrapPwWithNonce(kek, vaultUid, salt, nonceWrappedPw, fromHex(fixture.WRAPPED_PW));
    const viaRc = unwrapRcWithNonce(kekRecovery, vaultUid, nonceWrappedRc, fromHex(fixture.WRAPPED_RC));

    expect(toHex(viaPw)).toBe(fixture.VAULT_KEY);
    expect(toHex(viaRc)).toBe(fixture.VAULT_KEY);
    expect(toHex(viaPw)).toBe(toHex(vaultKey));
    expect(fixture.UNWRAP).toBe('OK');
  }, 60_000);

  it('deriva las seis subclaves y la pubkey de identidad', () => {
    const keys = deriveSubkeys(vaultKey, salt);

    expect(toHex(keys.content)).toBe(fixture.K_CONTENT);
    expect(toHex(keys.nonce)).toBe(fixture.K_NONCE);
    expect(toHex(keys.path)).toBe(fixture.K_PATH);
    expect(toHex(keys.meta)).toBe(fixture.K_META);
    expect(toHex(keys.bind)).toBe(fixture.K_BIND);
    expect(toHex(keys.sign)).toBe(fixture.K_SIGN);

    expect(toHex(identityFromSubkeys(keys).pubkey)).toBe(fixture.PUBKEY);
  });

  it('deriva el path_id y descifra el path_enc del vector', () => {
    const keys = deriveSubkeys(vaultKey, salt);
    const id = pathId(keys, path);

    expect(toHex(id)).toBe(fixture.PATH_ID);
    expect(decryptPathEncWithNonce(keys, noncePath, id, fromHex(fixture.PATH_ENC))).toBe(path);
    expect(fixture.PATH_ENC_RT).toBe('OK');
  });

  it('cifra el contenido con el sobre ADR-0014 y reproduce NONCE_BODY/BLOB/BLOB_ID', () => {
    const keys = deriveSubkeys(vaultKey, salt);

    expect(toHex(nonceBody(keys, plaintext))).toBe(fixture.NONCE_BODY);

    const blob = encryptBlob(keys, plaintext);
    expect(toHex(blob)).toBe(fixture.BLOB);
    expect(String(blob.length)).toBe(fixture.BLOB_LEN);
    expect(toHex(blobId(blob))).toBe(fixture.BLOB_ID);

    // El camino del lector: solo el blob almacenado, sin el plaintext.
    expect(decryptBlob(keys, blob)).toEqual(plaintext);
    expect(fixture.BLOB_RT).toBe('OK');
  });

  it('firma el binding (T_MAC) y el registro (SIG), y verifica', () => {
    const keys = deriveSubkeys(vaultKey, salt);
    const identity = identityFromSubkeys(keys);
    const id = pathId(keys, path);
    const blob = encryptBlob(keys, plaintext);
    const hash = blobId(blob);

    const binding = tMac(identity, vaultUid, id, hash, epoch);
    expect(toHex(binding)).toBe(fixture.T_MAC);

    const sig = signRecord(identity, id, hash, binding, opCode, size, mtime, expectedRev, operationId, epoch);
    expect(toHex(sig)).toBe(fixture.SIG);

    expect(verifyRecord(identity.pubkey, id, hash, binding, opCode, size, mtime, expectedRev, operationId, epoch, sig)).toBe(true);
    expect(fixture.SIG_VERIFY).toBe('OK');
  });

  it('rechaza los negativos congenados', () => {
    const keys = deriveSubkeys(vaultKey, salt);
    const identity = identityFromSubkeys(keys);
    const kek = deriveKekVector(password);
    const kekRecovery = deriveKekRecovery(recoverySecret, salt);

    // NEG_WRONG_PW
    const wrongKek = deriveKekVector(`${password}x`);
    expect(() =>
      unwrapPwWithNonce(wrongKek, vaultUid, salt, nonceWrappedPw, fromHex(fixture.WRAPPED_PW)),
    ).toThrow();
    expect(fixture.NEG_WRONG_PW).toBe('REJECTED');

    // NEG_AAD: el mismo ciphertext con el AAD de recovery.
    expect(() =>
      cryptoProvider().xchachaOpen(kek, nonceWrappedPw, vaultkeyRcAad(vaultUid), fromHex(fixture.WRAPPED_PW)),
    ).toThrow();
    expect(fixture.NEG_AAD).toBe('REJECTED');

    // NEG_SWAP_PATH: ciphertext de otro path con el AAD de este.
    const otherPath = 'otro.md';
    const otherEnc = pathEnc(keys, otherPath);
    const swapId = pathId(keys, path);
    expect(() =>
      decryptPathEncWithNonce(keys, otherEnc.subarray(0, 24), swapId, otherEnc.subarray(24)),
    ).toThrow();
    expect(fixture.NEG_SWAP_PATH).toBe('REJECTED');

    // NEG_SIG: firma alterada.
    const id = pathId(keys, path);
    const blob = encryptBlob(keys, plaintext);
    const hash = blobId(blob);
    const binding = tMac(identity, vaultUid, id, hash, epoch);
    const sig = signRecord(identity, id, hash, binding, opCode, size, mtime, expectedRev, operationId, epoch);
    const altered = Uint8Array.from(sig);
    altered[altered.length - 1]! ^= 0x01;
    expect(verifyRecord(identity.pubkey, id, hash, binding, opCode, size, mtime, expectedRev, operationId, epoch, altered)).toBe(false);
    expect(fixture.NEG_SIG).toBe('REJECTED');

    // NEG_TRUNC: blob truncado.
    const truncated = blob.subarray(0, blob.length - 1);
    expect(() => decryptBlob(keys, truncated)).toThrow();
    expect(fixture.NEG_TRUNC).toBe('REJECTED');
  }, 60_000);

  it('dedupe: mismo contenido ⇒ mismo blob_id; distinto ⇒ distinto; rename no toca el blob', () => {
    const keys = deriveSubkeys(vaultKey, salt);

    const blob = encryptBlob(keys, plaintext);
    const again = encryptBlob(keys, plaintext);
    expect(toHex(blobId(again))).toBe(toHex(blobId(blob)));
    expect(fixture.DEDUPE_SAME).toBe('SAME');

    const other = new TextEncoder().encode('# nota de prueba\nsin acentos rotos: é\n');
    const otherBlob = encryptBlob(keys, other);
    expect(toHex(blobId(otherBlob))).not.toBe(toHex(blobId(blob)));
    expect(fixture.DEDUPE_DIFF).toBe('DIFFERENT');

    expect(toHex(blobId(encryptBlob(keys, plaintext)))).toBe(toHex(blobId(blob)));
    expect(toHex(pathId(keys, 'otro.md'))).not.toBe(toHex(pathId(keys, path)));
    expect(fixture.RENAME).toBe('OK');
  });

  it('un dispositivo NUEVO (solo material público + password) descifra un blob ajeno (ADR-0014)', () => {
    // Registro A: envuelve con la KEK real y cifra.
    const kek = deriveKekVector(password);
    const wrapped = wrapKeyPw(vaultKey, kek, vaultUid, salt);
    const blob = encryptBlob(deriveSubkeys(vaultKey, salt), plaintext);

    // Registro B: no tiene vault_key ni subclaves; deriva todo desde el material.
    const kekB = deriveKekVector(password);
    const vaultKeyB = unwrapKeyPw(wrapped, kekB, vaultUid, salt);
    const keysB = deriveSubkeys(vaultKeyB, salt);

    expect(decryptBlob(keysB, blob)).toEqual(plaintext);
  }, 60_000);

  it('roundtrip de envoltura de recovery y de path_enc con sobre aleatorio', () => {
    const kekRecovery = deriveKekRecovery(recoverySecret, salt);
    const wrapped = wrapKeyRc(vaultKey, kekRecovery, vaultUid);
    expect(toHex(unwrapKeyRc(wrapped, kekRecovery, vaultUid))).toBe(toHex(vaultKey));

    const keys = deriveSubkeys(vaultKey, salt);
    const stored = pathEnc(keys, path);
    expect(decryptPathEnc(keys, stored, pathId(keys, path))).toBe(path);
  }, 60_000);

  it('formatea operation_id como UUID textual', () => {
    expect(bytesToUuid(operationId)).toBe('00112233-4455-6677-8899-aabbccddeeff');
  });
});

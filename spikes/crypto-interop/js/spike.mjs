// Fase 0 — spike de cripto sync2 (Node ESM, JS puro, sin WASM)

const load = async (cands) => {
  for (const c of cands) {
    try {
      return await import(c);
    } catch (_) {
      /* siguiente */
    }
  }
  throw new Error('no se pudo importar: ' + cands.join(', '));
};

const argonMod = await load([
  '@noble/hashes/argon2.js',
  '@noble/hashes/argon2',
]).catch(() => {
  throw new Error('argon2 no disponible en @noble/hashes');
});

const hkdfMod = await load([
  '@noble/hashes/hkdf.js',
  '@noble/hashes/hkdf',
]);
const sha2 = await load([
  '@noble/hashes/sha2.js',
  '@noble/hashes/sha2',
]);
const hmacMod = await load([
  '@noble/hashes/hmac.js',
  '@noble/hashes/hmac',
]);
const chacha = await load([
  '@noble/ciphers/chacha.js',
  '@noble/ciphers/chacha',
]);
const curves = await load([
  '@noble/curves/ed25519.js',
  '@noble/curves/ed25519',
]);

const argon2id = argonMod.argon2id;
if (typeof argon2id !== 'function') {
  throw new Error('argon2 no disponible en @noble/hashes');
}

const hkdf = hkdfMod.hkdf;
const sha256 = sha2.sha256;
const hmac = hmacMod.hmac;
const xchacha20poly1305 = chacha.xchacha20poly1305;
const ed25519 = curves.ed25519;

const toHex = (u8) => Buffer.from(u8).toString('hex');
const enc = new TextEncoder();
const DOMAIN = 'sync2';

const concat = (...parts) => {
  const total = parts.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(total);
  let off = 0;
  for (const part of parts) {
    out.set(part, off);
    off += part.length;
  }
  return out;
};

const u8 = (v) => Uint8Array.of(v & 0xff);

const u16be = (v) => {
  const out = new Uint8Array(2);
  new DataView(out.buffer).setUint16(0, v, false);
  return out;
};

const u32be = (v) => {
  const out = new Uint8Array(4);
  new DataView(out.buffer).setUint32(0, v, false);
  return out;
};

const u64be = (v) => {
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, BigInt(v), false);
  return out;
};

const equalBytes = (a, b) =>
  a.length === b.length && a.every((v, i) => v === b[i]);

const hkdf32 = (ikm, salt, label) =>
  hkdf(sha256, ikm, salt, enc.encode(`${DOMAIN}/${label}`), 32);

const password = 'contraseña-e\u0301-東京';
const pwNfkc = password.normalize('NFKC');
const salt = Uint8Array.from(
  Buffer.from('000102030405060708090a0b0c0d0e0f', 'hex'),
);
const vaultId = Uint8Array.from(
  Buffer.from('0102030405060708090a0b0c0d0e0f10', 'hex'),
);
const vaultKey = Uint8Array.from(
  Array.from({ length: 32 }, (_, i) => i),
);
const recSecret = Uint8Array.from(
  Array.from({ length: 32 }, (_, i) => i + 0x20),
);

const m = 65536;
const t = 3;
const p = 4;
const dkLen = 32;

const path = '02-hermes-docs/hermes-core/obsidian-sync-plugin-plan.md';
const pathNfkc = path.normalize('NFKC');
const plaintext = enc.encode(
  '# nota de prueba\nsin acentos rotos: ñ\n',
);

const epoch = 1;
const opCode = 0x02;
const size = 39;
const mtime = 1758831000000n;
const expectedRev = 1834n;
const operationId = Uint8Array.from(
  Buffer.from('00112233445566778899aabbccddeeff', 'hex'),
);

const nonceWrappedPw = new Uint8Array(24).fill(0x01);
const nonceWrappedRc = new Uint8Array(24).fill(0x02);
const noncePath = new Uint8Array(24).fill(0x03);

const kek = argon2id(
  enc.encode(pwNfkc),
  salt,
  { m, t, p, dkLen },
);

const kekRecovery = hkdf(
  sha256,
  recSecret,
  salt,
  enc.encode('sync2/kek-recovery'),
  32,
);

const aadPw = concat(
  enc.encode('sync2/vaultkey/pw'),
  vaultId,
  salt,
  u32be(m),
  u32be(t),
  u32be(p),
  u32be(dkLen),
);

const aadRc = concat(
  enc.encode('sync2/vaultkey/rc'),
  vaultId,
);

const wrappedPw = xchacha20poly1305(
  kek,
  nonceWrappedPw,
  aadPw,
).encrypt(vaultKey);

const wrappedRc = xchacha20poly1305(
  kekRecovery,
  nonceWrappedRc,
  aadRc,
).encrypt(vaultKey);

const unwrappedPw = xchacha20poly1305(
  kek,
  nonceWrappedPw,
  aadPw,
).decrypt(wrappedPw);

const unwrappedRc = xchacha20poly1305(
  kekRecovery,
  nonceWrappedRc,
  aadRc,
).decrypt(wrappedRc);

const unwrapStatus =
  equalBytes(unwrappedPw, vaultKey) &&
  equalBytes(unwrappedRc, vaultKey)
    ? 'OK'
    : 'FAIL';

const kContent = hkdf32(vaultKey, salt, 'content');
const kNonce = hkdf32(vaultKey, salt, 'nonce');
const kPath = hkdf32(vaultKey, salt, 'path');
const kMeta = hkdf32(vaultKey, salt, 'meta');
const kBind = hkdf32(vaultKey, salt, 'bind');
const kSign = hkdf32(vaultKey, salt, 'sign');

const publicKey = ed25519.getPublicKey(kSign);

const pathBytes = enc.encode(pathNfkc);
const pathId = hmac(sha256, kPath, pathBytes);

const pathEnc = xchacha20poly1305(
  kPath,
  noncePath,
  pathId,
).encrypt(pathBytes);

const pathEncRt = xchacha20poly1305(
  kPath,
  noncePath,
  pathId,
).decrypt(pathEnc);

const nonceBody = hmac(
  sha256,
  kNonce,
  plaintext,
).slice(0, 24);

// ADR-0014: el blob almacenado es nonce[24] ‖ ciphertext ‖ tag[16] — el nonce viaja adentro.
const blob = concat(
  nonceBody,
  xchacha20poly1305(kContent, nonceBody, new Uint8Array(0)).encrypt(plaintext),
);

const blobRt = xchacha20poly1305(
  kContent,
  blob.slice(0, 24),
  new Uint8Array(0),
).decrypt(blob.slice(24));

const blobId = sha256(blob);

const msgBind = concat(
  enc.encode('sync2/bind'),
  vaultId,
  pathId,
  blobId,
  u16be(epoch),
);

const tMac = ed25519.sign(msgBind, kSign);

const msgRec = concat(
  enc.encode('sync2/rec'),
  u8(opCode),
  pathId,
  blobId,
  tMac,
  u64be(size),
  u64be(mtime),
  u64be(expectedRev),
  operationId,
  u16be(epoch),
);

const sig = ed25519.sign(msgRec, kSign);
const sigVerify = ed25519.verify(sig, msgRec, publicKey)
  ? 'OK'
  : 'REJECTED';

const wrongPassword = `${password}x`.normalize('NFKC');

let negWrongPw = 'LEAKED';
try {
  const wrongKek = argon2id(
    enc.encode(wrongPassword),
    salt,
    { m, t, p, dkLen },
  );
  xchacha20poly1305(
    wrongKek,
    nonceWrappedPw,
    aadPw,
  ).decrypt(wrappedPw);
} catch (_) {
  negWrongPw = 'REJECTED';
}

let negAad = 'LEAKED';
try {
  xchacha20poly1305(
    kek,
    nonceWrappedPw,
    aadRc,
  ).decrypt(wrappedPw);
} catch (_) {
  negAad = 'REJECTED';
}

let negSwapPath = 'LEAKED';
try {
  const otherPathBytes = enc.encode('otro.md');
  const otherPathId = hmac(sha256, kPath, otherPathBytes);
  const otherPathEnc = xchacha20poly1305(
    kPath,
    noncePath,
    otherPathId,
  ).encrypt(otherPathBytes);

  xchacha20poly1305(
    kPath,
    noncePath,
    pathId,
  ).decrypt(otherPathEnc);
} catch (_) {
  negSwapPath = 'REJECTED';
}

let negSig = 'LEAKED';
try {
  const alteredSig = sig.slice();
  alteredSig[alteredSig.length - 1] ^= 0x01;
  if (!ed25519.verify(alteredSig, msgRec, publicKey)) {
    throw new Error('invalid signature');
  }
} catch (_) {
  negSig = 'REJECTED';
}

let negTrunc = 'LEAKED';
try {
  const truncatedBlob = blob.slice(0, -1);
  xchacha20poly1305(
    kContent,
    blob.slice(0, 24),
    new Uint8Array(0),
  ).decrypt(truncatedBlob.slice(24));
} catch (_) {
  negTrunc = 'REJECTED';
}

const blobAgain = concat(
  nonceBody,
  xchacha20poly1305(kContent, nonceBody, new Uint8Array(0)).encrypt(plaintext),
);
const blobAgainId = sha256(blobAgain);

const otherPlaintext = enc.encode(
  '# nota de prueba\nsin acentos rotos: é\n',
);
const otherNonceBody = hmac(
  sha256,
  kNonce,
  otherPlaintext,
).slice(0, 24);
const otherBlob = concat(
  otherNonceBody,
  xchacha20poly1305(kContent, otherNonceBody, new Uint8Array(0)).encrypt(otherPlaintext),
);
const otherBlobId = sha256(otherBlob);

const dedupeSame = equalBytes(blobId, blobAgainId)
  ? 'SAME'
  : 'DIFFERENT';
const dedupeDiff = equalBytes(blobId, otherBlobId)
  ? 'SAME'
  : 'DIFFERENT';

const renamePath = enc.encode('otro.md');
const renamePathId = hmac(sha256, kPath, renamePath);
const renameOk =
  equalBytes(blobId, sha256(blob)) &&
  !equalBytes(pathId, renamePathId)
    ? 'OK'
    : 'FAIL';

const out = [
  ['PW_NFKC', pwNfkc],
  ['KEK', toHex(kek)],
  ['VAULT_KEY', toHex(vaultKey)],
  ['WRAPPED_PW', toHex(wrappedPw)],
  ['WRAPPED_RC', toHex(wrappedRc)],
  ['UNWRAP', unwrapStatus],
  ['K_CONTENT', toHex(kContent)],
  ['K_NONCE', toHex(kNonce)],
  ['K_PATH', toHex(kPath)],
  ['K_META', toHex(kMeta)],
  ['K_BIND', toHex(kBind)],
  ['K_SIGN', toHex(kSign)],
  ['PUBKEY', toHex(publicKey)],
  ['PATH_ID', toHex(pathId)],
  ['PATH_ENC', toHex(pathEnc)],
  ['PATH_ENC_RT', equalBytes(pathEncRt, pathBytes) ? 'OK' : 'FAIL'],
  ['NONCE_BODY', toHex(nonceBody)],
  ['BLOB', toHex(blob)],
  ['BLOB_LEN', String(blob.length)],
  ['BLOB_ID', toHex(blobId)],
  ['BLOB_RT', equalBytes(blobRt, plaintext) ? 'OK' : 'FAIL'],
  ['T_MAC', toHex(tMac)],
  ['SIG', toHex(sig)],
  ['SIG_VERIFY', sigVerify],
  ['NEG_WRONG_PW', negWrongPw],
  ['NEG_AAD', negAad],
  ['NEG_SWAP_PATH', negSwapPath],
  ['NEG_SIG', negSig],
  ['NEG_TRUNC', negTrunc],
  ['DEDUPE_SAME', dedupeSame],
  ['DEDUPE_DIFF', dedupeDiff],
  ['RENAME', renameOk],
];

for (const [key, value] of out) {
  console.log(`${key}\t${value}`);
}

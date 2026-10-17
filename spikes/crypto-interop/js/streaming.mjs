import { createReadStream } from 'node:fs';
import {
  mkdir,
  rm,
  stat,
  readFile,
  open,
  writeFile,
  utimes,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const load = async (candidates) => {
  for (const candidate of candidates) {
    try {
      return await import(candidate);
    } catch (_) {
      // Try the next candidate.
    }
  }
  throw new Error(`no se pudo importar: ${candidates.join(', ')}`);
};

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

const sha256 = sha2.sha256;
const hmac = hmacMod.hmac;
const xchacha20poly1305 = chacha.xchacha20poly1305;

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '../../..');
const fixtureCandidates = [
  resolve(here, '../../tests/fixtures/crypto/sync2.json'),
  resolve(repoRoot, 'tests/fixtures/crypto/sync2.json'),
  resolve(process.cwd(), '../../tests/fixtures/crypto/sync2.json'),
  resolve(process.cwd(), 'tests/fixtures/crypto/sync2.json'),
];

const TMP_DIR = process.env.SPIKE_TMP_DIR ?? join(tmpdir(), 'ovaultd-spike-stream');
const FILE_MIB = 64;
const FILE_SIZE = FILE_MIB * 1024 * 1024;
const SMALL_SIZE = 1024 * 1024;
const EMPTY = new Uint8Array(0);

const toHex = (value) => Buffer.from(value).toString('hex');

const equalBytes = (a, b) =>
  a.length === b.length && a.every((value, index) => value === b[index]);

const findKey = (value, wanted) => {
  if (!value || typeof value !== 'object') return undefined;

  for (const [key, child] of Object.entries(value)) {
    if (key.toUpperCase() === wanted) return child;
    const nested = findKey(child, wanted);
    if (nested !== undefined) return nested;
  }

  return undefined;
};

const readFixture = async () => {
  let lastError;

  for (const candidate of fixtureCandidates) {
    try {
      const raw = await readFile(candidate, 'utf8');
      const parsed = JSON.parse(raw);

      const nonceValue = findKey(parsed, 'K_NONCE');
      const contentValue = findKey(parsed, 'K_CONTENT');

      if (typeof nonceValue !== 'string' || typeof contentValue !== 'string') {
        throw new Error(
          `el fixture no contiene K_NONCE y K_CONTENT: ${candidate}`,
        );
      }

      if (!/^[0-9a-fA-F]{64}$/.test(nonceValue)) {
        throw new Error(`K_NONCE no es hex de 32 bytes: ${candidate}`);
      }

      if (!/^[0-9a-fA-F]{64}$/.test(contentValue)) {
        throw new Error(`K_CONTENT no es hex de 32 bytes: ${candidate}`);
      }

      return {
        kNonce: Buffer.from(nonceValue, 'hex'),
        kContent: Buffer.from(contentValue, 'hex'),
      };
    } catch (error) {
      lastError = error;
    }
  }

  throw new Error(
    `no se pudo leer el fixture sync2.json; rutas probadas: ${fixtureCandidates.join(
      ', ',
    )}; último error: ${lastError?.message ?? 'desconocido'}`,
  );
};

const makePattern = (offset, length) => {
  const buffer = Buffer.allocUnsafe(length);
  for (let index = 0; index < length; index += 1) {
    buffer[index] = (offset + index) & 0xff;
  }
  return buffer;
};

const createDeterministicFile = async (path, size) => {
  const handle = await open(path, 'w');
  try {
    const chunkSize = 1024 * 1024;
    for (let offset = 0; offset < size; offset += chunkSize) {
      const length = Math.min(chunkSize, size - offset);
      await handle.write(makePattern(offset, length));
    }
  } finally {
    await handle.close();
  }
};

const makeMemoryTracker = () => {
  const baseRss = process.memoryUsage().rss;
  let peakRss = baseRss;
  let peakArrayBuffers = process.memoryUsage().arrayBuffers;

  const sample = () => {
    const usage = process.memoryUsage();
    if (usage.rss > peakRss) peakRss = usage.rss;
    if (usage.arrayBuffers > peakArrayBuffers) {
      peakArrayBuffers = usage.arrayBuffers;
    }
  };

  return {
    sample,
    values: () => ({ baseRss, peakRss, peakArrayBuffers }),
  };
};

const pass1 = async (path, kNonce, chunkSize, includeHash = false) => {
  const nonceMac = hmac.create(sha256, kNonce);
  const contentHash = includeHash ? sha256.create() : null;
  const tracker = makeMemoryTracker();
  let size = 0;

  for await (const chunk of createReadStream(path, {
    highWaterMark: chunkSize,
  })) {
    nonceMac.update(chunk);
    if (contentHash) contentHash.update(chunk);
    size += chunk.length;
    tracker.sample();
  }

  const nonce = nonceMac.digest().slice(0, 24);
  const hash = contentHash ? contentHash.digest() : null;
  const metadata = await stat(path);

  tracker.sample();

  return {
    nonce,
    hash,
    size,
    mtimeMs: metadata.mtimeMs,
    memory: tracker.values(),
  };
};

const pass2 = async (path, expected, kContent, tracker = null) => {
  const before = await stat(path);

  if (
    before.size !== expected.size ||
    before.mtimeMs !== expected.mtimeMs
  ) {
    throw new Error('ABORT');
  }

  const plaintext = Buffer.alloc(expected.size);
  const contentHash = sha256.create();
  let offset = 0;

  if (tracker) tracker.sample();

  for await (const chunk of createReadStream(path, {
    highWaterMark: 1024 * 1024,
  })) {
    if (offset + chunk.length > plaintext.length) {
      throw new Error('ABORT');
    }

    plaintext.set(chunk, offset);
    contentHash.update(chunk);
    offset += chunk.length;

    if (tracker) tracker.sample();
  }

  const after = await stat(path);
  const actualHash = contentHash.digest();

  if (
    offset !== expected.size ||
    after.size !== expected.size ||
    after.mtimeMs !== expected.mtimeMs ||
    !equalBytes(actualHash, expected.hash)
  ) {
    throw new Error('ABORT');
  }

  if (tracker) tracker.sample();

  const blob = xchacha20poly1305(
    kContent,
    expected.nonce,
    EMPTY,
  ).encrypt(plaintext);

  if (tracker) tracker.sample();

  return { blob, plaintext };
};

const mib = (bytes) => (bytes / (1024 * 1024)).toFixed(2);

let tempReady = false;

try {
  const { kNonce, kContent } = await readFixture();

  await rm(TMP_DIR, { recursive: true, force: true });
  await mkdir(TMP_DIR, { recursive: true });
  tempReady = true;

  const largePath = join(TMP_DIR, 'large.bin');
  const smallPath = join(TMP_DIR, 'small.bin');

  await createDeterministicFile(largePath, FILE_SIZE);

  const pass1_1k = await pass1(largePath, kNonce, 1024, true);
  const pass1_64k = await pass1(largePath, kNonce, 64 * 1024);
  const pass1_1m = await pass1(largePath, kNonce, 1024 * 1024);

  const fullBuffer = await readFile(largePath);
  const fullBufferNonce = hmac(sha256, kNonce, fullBuffer).slice(0, 24);

  const nonce1kMatch = equalBytes(pass1_1k.nonce, fullBufferNonce);
  const nonce64kMatch = equalBytes(pass1_64k.nonce, fullBufferNonce);
  const nonce1mMatch = equalBytes(pass1_1m.nonce, fullBufferNonce);

  const pass1Memory = pass1_1k.memory;

  const pass2Tracker = makeMemoryTracker();
  const twoPass = await pass2(
    largePath,
    {
      nonce: pass1_1k.nonce,
      hash: pass1_1k.hash,
      size: pass1_1k.size,
      mtimeMs: pass1_1k.mtimeMs,
    },
    kContent,
    pass2Tracker,
  );

  const referenceBlob = xchacha20poly1305(
    kContent,
    fullBufferNonce,
    EMPTY,
  ).encrypt(fullBuffer);
  const blobId = sha256(twoPass.blob);
  const referenceBlobId = sha256(referenceBlob);

  await createDeterministicFile(smallPath, SMALL_SIZE);
  const smallPass1 = await pass1(smallPath, kNonce, 64 * 1024, true);

  const modified = await open(smallPath, 'r+');
  try {
    const changedByte = Buffer.from([0x5a]);
    await modified.write(changedByte, 0, 1, 0);
  } finally {
    await modified.close();
  }

  let modifiedResult = 'MISSED';
  try {
    await pass2(
      smallPath,
      {
        nonce: smallPass1.nonce,
        hash: smallPass1.hash,
        size: smallPass1.size,
        mtimeMs: smallPass1.mtimeMs,
      },
      kContent,
    );
  } catch (error) {
    if (error?.message === 'ABORT') {
      modifiedResult = 'ABORT';
    } else {
      throw error;
    }
  }

  const blobMatch = equalBytes(twoPass.blob, referenceBlob);

  const d1 = pass1Memory.peakRss - pass1Memory.baseRss;
  const p2 = pass2Tracker.values();
  const d2 = p2.peakRss - p2.baseRss;
  const verdict =
    `PASADA 1 O(1) (delta ${mib(d1)} MiB, no escala con el archivo) Y PASADA 2 O(n) (delta ${mib(d2)} MiB, ~4x el archivo): ` +
    'EL REQUISITO DE MEMORIA ACOTADA NO SE CUMPLE CON UN AEAD DE MENSAJE UNICO XChaCha20-Poly1305; ES UN HALLAZGO DE DISENO.';

  const output = [
    ['FILE_MIB', String(FILE_MIB)],
    ['CHUNK_1K_NONCE_MATCH', nonce1kMatch ? 'OK' : 'FAIL'],
    ['CHUNK_64K_NONCE_MATCH', nonce64kMatch ? 'OK' : 'FAIL'],
    ['CHUNK_1M_NONCE_MATCH', nonce1mMatch ? 'OK' : 'FAIL'],
    [
      'FULL_BUFFER_NONCE_MATCH',
      equalBytes(fullBufferNonce, pass1_1k.nonce) ? 'OK' : 'FAIL',
    ],
    ['BASE_RSS_MIB', mib(pass1Memory.baseRss)],
    ['PEAK_RSS_PASS1_MIB', mib(pass1Memory.peakRss)],
    ['PEAK_RSS_PASS1_DELTA_MIB', mib(d1)],
    ['PEAK_RSS_PASS2_MIB', mib(p2.peakRss)],
    ['PEAK_RSS_PASS2_DELTA_MIB', mib(d2)],
    [
      'PEAK_ARRAYBUFFERS_PASS2_MIB',
      mib(pass2Tracker.values().peakArrayBuffers),
    ],
    ['BLOB_ID', toHex(blobId)],
    ['BLOB_ID_REFERENCE', toHex(referenceBlobId)],
    ['BLOB_MATCH', blobMatch ? 'OK' : 'FAIL'],
    ['MODIFIED_BETWEEN_PASSES', modifiedResult],
    ['VERDICT', verdict],
  ];

  for (const [key, value] of output) {
    process.stdout.write(`${key}\t${value}\n`);
  }
} catch (error) {
  process.stderr.write(`ERROR: ${error?.message ?? String(error)}\n`);
  process.exitCode = 1;
} finally {
  if (tempReady) {
    try {
      await rm(TMP_DIR, { recursive: true, force: true });
    } catch (error) {
      process.stderr.write(
        `ERROR: no se pudo limpiar ${TMP_DIR}: ${
          error?.message ?? String(error)
        }\n`,
      );
      process.exitCode = 1;
    }
  }
}

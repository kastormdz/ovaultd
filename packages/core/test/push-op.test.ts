//! El wire de una operación de outbox: `move` tiene que publicar `prev_path_id` (el server
//! lo exige y su ausencia era un BadRequest FATAL que congelaba la cola para siempre).

import {
  cryptoProvider,
  deriveSubkeys,
  pathId as calcularPathId,
  randomSalt,
  randomVaultKey,
} from '@ovaultd/crypto';
import { b64Encode, fromHex, toHex } from '@ovaultd/protocol';
import { describe, expect, it } from 'vitest';

import { prepareOutboxEntry } from '../src/outbox.js';
import { construirOp, type PushDeps } from '../src/push.js';

const CLAVES = deriveSubkeys(randomVaultKey(), randomSalt());
const VAULT_UUID = new Uint8Array(16).fill(0xaa);
const ORIGEN = 'Notas/vieja.md';
const DESTINO = 'Notas/nueva.md';
const CONTENIDO = new TextEncoder().encode('contenido de la nota');

const pathIdDe = (ruta: string): string => toHex(calcularPathId(CLAVES, ruta));
const hashDe = (bytes: Uint8Array): string => toHex(cryptoProvider().sha256(bytes));

const deps = (): PushDeps => {
  const puestos: string[] = [];
  return {
    conn: {
      putBlob: async (blobId: string) => {
        puestos.push(blobId);
      },
    },
    keys: CLAVES,
    vaultUuid: VAULT_UUID,
    epoch: 1,
    puestos,
  } as unknown as PushDeps;
};

describe('construirOp', () => {
  it('un move publica el prev_path_id del origen', async () => {
    const entry = prepareOutboxEntry({
      kind: 'move',
      path: DESTINO,
      pathId: pathIdDe(DESTINO),
      prevPathId: pathIdDe(ORIGEN),
      expectedRev: 5,
      contentB64: b64Encode(CONTENIDO),
      contentHash: hashDe(CONTENIDO),
      mtimeMs: 1,
    });

    const { op } = await construirOp(deps(), entry, 5, new Uint8Array(16).fill(7));

    expect(op.op).toBe('move');
    expect(op.path_id).toBe(b64Encode(fromHex(pathIdDe(DESTINO))));
    expect(op.prev_path_id).toBe(b64Encode(fromHex(pathIdDe(ORIGEN))));
  });

  it('un mod no lleva prev_path_id', async () => {
    const entry = prepareOutboxEntry({
      kind: 'mod',
      path: DESTINO,
      pathId: pathIdDe(DESTINO),
      expectedRev: 5,
      contentB64: b64Encode(CONTENIDO),
      contentHash: hashDe(CONTENIDO),
      mtimeMs: 1,
    });

    const { op } = await construirOp(deps(), entry, 5, new Uint8Array(16).fill(7));

    expect(op.op).toBe('mod');
    expect(op.prev_path_id).toBeNull();
  });
});

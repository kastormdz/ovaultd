//! Cuarentena de la outbox: una entrada que el server rechaza por cómo ES (archivo
//! demasiado grande, bad_request, sin snapshot) no puede frenar la cola para siempre.
//! Antes, `pushOnce` cortaba el ciclo entero y la misma entrada volvía a fallar en cada
//! pasada, dejando sin publicar todo lo que estaba detrás.

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
import { pushOnce, type PushDeps } from '../src/push.js';
import type { OutboxEntry } from '../src/StateStore.js';
import { SyncRejectedError } from '../src/SyncConnection.js';

const CLAVES = deriveSubkeys(randomVaultKey(), randomSalt());

const entrada = (ruta: string, texto: string): OutboxEntry => {
  const bytes = new TextEncoder().encode(texto);
  return prepareOutboxEntry({
    kind: 'add',
    path: ruta,
    pathId: toHex(calcularPathId(CLAVES, ruta)),
    expectedRev: 0,
    contentB64: b64Encode(bytes),
    contentHash: toHex(cryptoProvider().sha256(bytes)),
    mtimeMs: 1,
  });
};

describe('pushOnce: cuarentena', () => {
  it('una entrada rechazada no impide publicar las demás', async () => {
    const grande = entrada('video.mp4', 'x'.repeat(200));
    const chica = entrada('nota.md', 'hola');

    const estados = new Map<string, string>();
    const store = {
      readCheckpoint: async () => ({ since: 0, minRev: null, updatedAt: 1 }),
      listOutbox: async () => [grande, chica],
      listIndex: async () => [],
      listBase: async () => [],
      putOutboxMany: async () => {},
      getOutbox: async () => null,
      setOutboxState: async (id: string, state: string, motivo?: string | null) => {
        estados.set(id, motivo ? `${state}:${motivo}` : state);
      },
      confirmarLote: async () => {},
    };
    const port = {
      list: async () => [],
      stat: async () => null,
      read: async () => new Uint8Array(),
    };
    const grandePathId = b64Encode(fromHex(grande.pathId));
    const conn = {
      putBlob: async () => {},
      commit: async (batch: { batch_id: string; ops: Array<{ path_id: string }> }) => {
        if (batch.ops.some((op) => op.path_id === grandePathId)) {
          throw new SyncRejectedError('max_file_exceeded', 'supera 32 MiB');
        }
        return { t: 'batch_ack', batch_id: batch.batch_id, rev: 1 };
      },
    };

    const deps = {
      store,
      port,
      keys: CLAVES,
      conn,
      vaultUuid: new Uint8Array(16).fill(0xaa),
      epoch: 1,
    } as unknown as PushDeps;

    const report = await pushOnce(deps);

    expect(report.rechazadas).toBe(1);
    expect(report.empujadas).toBe(1);
    expect(estados.get(grande.operationId)).toContain('rejected');
    expect(estados.get(grande.operationId)).toContain('max_file_exceeded');
    expect(estados.get(chica.operationId)).toBe('sent');
  });
});

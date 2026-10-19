/**
 * Modos del cliente (plan fase 3, paso 5 y aceptación ejecutable):
 *  - `pull_only_emits_zero_pushes`: un dispositivo que solo baja jamás publica.
 *  - `fresh_empty_client_cannot_delete_remote`: un cliente sin archivos locales no borra
 *    el vault remoto (vault recién creado, disco sin montar, scan a medias).
 */

import { deriveSubkeys, randomSalt, randomVaultKey } from '@ovaultd/crypto';
import { describe, expect, it } from 'vitest';

import type { FileSystemPort } from '../src/file-port.js';
import { pushOnce } from '../src/push.js';
import { correrCiclo } from '../src/SyncClient.js';
import type { SyncConnection } from '../src/SyncConnection.js';

const CLAVES = deriveSubkeys(randomVaultKey(), randomSalt());

const CHECKPOINT = { since: 2, minRev: null, updatedAt: 1_700_000_000_000 };

describe('modos del ciclo', () => {
  it('pull_only_emits_zero_pushes', async () => {
    const escrituras: string[] = [];
    const commits: string[] = [];
    const store = {
      readCheckpoint: async () => CHECKPOINT,
      listOutbox: async () => [],
      listIndex: async () => [],
      listBase: async () => [],
      writeCheckpoint: async () => void escrituras.push('checkpoint'),
      putOutbox: async () => void escrituras.push('outbox'),
      setOutboxState: async () => void escrituras.push('outbox-state'),
    };
    const conn = {
      pull: async (since: number) => ({ items: [], next_since: since, min_rev: null }),
      commit: async () => {
        commits.push('commit');
        return { rev: 3 };
      },
    };

    const report = await correrCiclo({
      store,
      port: {} as FileSystemPort,
      keys: CLAVES,
      conn: conn as unknown as SyncConnection,
      vaultUuid: new Uint8Array(16),
      epoch: 0,
      modo: 'pull-only',
    } as never);

    expect(report.push).toBeNull(); // no se intentó publicar
    expect(commits).toHaveLength(0); // jamás tocó el WS de commit
    expect(escrituras).not.toContain('outbox'); // no encoló nada
    expect(escrituras).not.toContain('outbox-state');
    expect(report.estado).toBe('convergido');
  });

  it('fresh_empty_client_cannot_delete_remote', async () => {
    // La base conoce una nota; el disco está VACÍO: el diff propone borrarla, la guarda
    // lo bloquea y el report lo dice (nada llega a la outbox).
    const enBase = [
      {
        pathId: 'a'.repeat(64),
        path: 'Nota.md',
        blobId: 'b'.repeat(64),
        contentHash: 'c'.repeat(64),
        rev: 2,
      },
    ];
    const escrituras: string[] = [];
    const store = {
      readCheckpoint: async () => CHECKPOINT,
      listBase: async () => enBase,
      listIndex: async () => enBase,
      listOutbox: async () => [],
      getOutbox: async () => undefined,
      putOutbox: async () => void escrituras.push('outbox'),
      writeCheckpoint: async () => void escrituras.push('checkpoint'),
      setOutboxState: async () => void escrituras.push('outbox-state'),
    };
    const port = {
      list: async () => [],
    } as unknown as FileSystemPort;
    const conn = { commit: async () => ({ rev: 3 }) };

    const report = await pushOnce({
      store,
      port,
      keys: CLAVES,
      conn: conn as unknown as SyncConnection,
      vaultUuid: new Uint8Array(16),
      epoch: 0,
    } as never);

    expect(report.deletesBloqueados).toBe(1);
    expect(report.empujadas).toBe(0);
    expect(report.diff.borrados).toBe(0); // el borrado no se encoló
    expect(escrituras).not.toContain('outbox');
  });
});

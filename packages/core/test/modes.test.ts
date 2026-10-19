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

  it('las operaciones pendientes suben agrupadas en UN batch (una sola revisión por lote)', async () => {
    // Tres archivos nuevos en disco, outbox vacía: el push los encola y los manda en un
    // solo commit. El server agrupa las ops bajo UNA revisión (el modelo del restore
    // masivo): todas las entradas se confirman con el mismo `ack.rev`.
    const archivos = ['a.md', 'b.md', 'c.md'];
    const bases = new Map<string, { pathId: string; rev: number }>();
    const outbox = new Map<string, { operationId: string; state: string }>();
    const commits: Array<{ ops: unknown[]; expected_rev: number }> = [];
    const store = {
      readCheckpoint: async () => ({ since: 0, minRev: null, updatedAt: 0 }),
      listBase: async () => [],
      listIndex: async () => [],
      listOutbox: async () => [...outbox.values()],
      getOutbox: async (id: string) => outbox.get(id),
      putOutbox: async (entry: { operationId: string; state: string }) =>
        void outbox.set(entry.operationId, entry),
      deleteOutbox: async (id: string) => void outbox.delete(id),
      setOutboxState: async (id: string, state: string) => {
        const entry = outbox.get(id);
        if (entry) outbox.set(id, { ...entry, state });
      },
      getIndex: async () => null,
      writeCheckpoint: async () => undefined,
      putBase: async (entrada: { pathId: string; rev: number }) => void bases.set(entrada.pathId, entrada),
      putIndex: async () => undefined,
    };
    const port = {
      list: async () => archivos.map((name) => ({ name, isFile: true, isDirectory: false })),
      read: async (ruta: string) => new TextEncoder().encode(`contenido ${ruta}`),
      stat: async () => ({ size: 12, mtimeMs: 1_700_000_000_000, isFile: true, isDirectory: false }),
    } as unknown as FileSystemPort;
    const conn = {
      putBlob: async () => undefined,
      commit: async (batch: { ops: unknown[]; expected_rev: number; t: string }) => {
        commits.push(batch);
        // Una sola revisión por batch (como el server real).
        return { rev: batch.expected_rev + 1 };
      },
    };

    const report = await pushOnce({
      store,
      port,
      keys: CLAVES,
      conn: conn as unknown as SyncConnection,
      vaultUuid: new Uint8Array(16),
      epoch: 0,
    } as never);

    expect(report.empujadas).toBe(3);
    expect(commits).toHaveLength(1); // UN solo batch para las tres ops
    expect(commits[0]!.ops).toHaveLength(3);
    expect(report.headRev).toBe(1);
    expect(bases.size).toBe(3); // una base por op…
    expect([...bases.values()].map((b) => b.rev)).toEqual([1, 1, 1]); // …todas en la rev del lote
  });
});

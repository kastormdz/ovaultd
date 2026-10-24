/**
 * El ciclo de convergencia (reconcile) — fase 5, aceptación ejecutable: cuando el cursor
 * quedó por debajo del piso de retención del server (`min_rev > since`), el pull NO puede
 * reconstruir el diff incremental: se informa `resync-required` SIN borrar nada — el
 * estado local (outbox incluida) se preserva y lo resuelve el llamador. Jamás "arreglar"
 * el cursor tocando el estado, y jamás aplicar media delta.
 */

import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApplyDeps } from '../src/apply-runner.js';
import { applyRevision } from '../src/apply-runner.js';
import { pullOnce } from '../src/reconcile.js';
import type { SyncConnection } from '../src/SyncConnection.js';

vi.mock('../src/apply-runner.js', () => ({ applyRevision: vi.fn() }));

describe('reconcile: piso de retención (min_rev)', () => {
  it('cursor vencido: informa resync y preserva outbox, checkpoint y vault', async () => {
    const escrituras: string[] = [];
    const outbox = [{ operationId: 'op-pendiente-1', kind: 'mod', ruta: 'Nota.md' }];
    const store = {
      readCheckpoint: async () => ({ since: 3, minRev: null, updatedAt: 1_700_000_000_000 }),
      listOutbox: async () => outbox,
      writeCheckpoint: async () => void escrituras.push('checkpoint'),
      writeOutbox: async () => void escrituras.push('outbox'),
      clearOutbox: async () => void escrituras.push('clear-outbox'),
    };
    const port = {
      escribir: async () => void escrituras.push('vault:fs'),
      borrar: async () => void escrituras.push('vault:fs'),
    };
    let desde: number | null = null;
    const conn = {
      pull: async (since: number) => {
        desde = since;
        // El server: la historia anterior a la rev 5 ya no está (retención).
        return { items: [], next_since: since, min_rev: 5 };
      },
    };

    const report = await pullOnce(conn as unknown as SyncConnection, {
      store,
      port,
      keys: {},
    } as unknown as ApplyDeps);

    expect(report.status).toBe('resync-required');
    expect(report.resync).toEqual({ since: 3, minRev: 5 });
    expect(report.applied).toBe(0);
    expect(desde).toBe(3); // pidió desde SU cursor, no desde el piso
    expect(escrituras).toEqual([]); // no tocó el checkpoint, la outbox ni el vault
    expect(await store.listOutbox()).toHaveLength(1); // la op local sigue pendiente
  });
});

describe('reconcile: el cursor avanza por revisión completa', () => {
  const registro = (rev: number, byte: number) =>
    ({
      t: 'record',
      rev,
      op: 'del',
      path_id: Buffer.from(new Uint8Array(32).fill(byte)).toString('base64'),
      path_enc: null,
      blob_id: null,
      t_mac: null,
      sig: '',
      epoch: 1,
    }) as never;

  const armar = (items: unknown[], nextSince: number) => {
    const escrituras: Array<{ since: number; minRev: number | null }> = [];
    const store = {
      readCheckpoint: async () => ({ since: 6, minRev: null, updatedAt: 1 }),
      getIndex: async () => null,
      writeCheckpoint: async (entrada: { since: number; minRev: number | null }) => {
        escrituras.push(entrada);
      },
    };
    // El server real no re-entrega lo que ya quedó atrás del cursor.
    let primera = true;
    const conn = {
      pull: async (since: number) => {
        if (primera && since < nextSince) {
          primera = false;
          return { items, next_since: nextSince, min_rev: null };
        }
        return { items: [], next_since: nextSince, min_rev: null };
      },
      getBlob: async () => new Uint8Array([1]),
    };
    return { escrituras, store, conn };
  };

  beforeEach(() => {
    vi.mocked(applyRevision).mockReset();
  });

  it('una revisión con 3 operaciones mueve el cursor recién al aplicar la tercera', async () => {
    const { escrituras, store, conn } = armar(
      [registro(7, 0x10), registro(7, 0x11), registro(7, 0x12)],
      7,
    );
    vi.mocked(applyRevision).mockResolvedValue({ status: 'applied', intentId: 'i' });

    const report = await pullOnce(conn as unknown as SyncConnection, { store, port: {}, keys: {} } as never);

    expect(report.status).toBe('pulled');
    expect(report.applied).toBe(3);
    expect(escrituras.map((e) => e.since)).toEqual([7]);
  });

  it('si la segunda operación bloquea, el cursor NO pasa la revisión', async () => {
    const { escrituras, store, conn } = armar(
      [registro(7, 0x10), registro(7, 0x11), registro(7, 0x12)],
      7,
    );
    vi.mocked(applyRevision)
      .mockResolvedValueOnce({ status: 'applied', intentId: 'i' })
      .mockResolvedValueOnce({ status: 'blocked', intentId: 'b', reason: 'tercera versión' })
      .mockResolvedValue({ status: 'applied', intentId: 'j' });

    const report = await pullOnce(conn as unknown as SyncConnection, { store, port: {}, keys: {} } as never);

    expect(report.status).toBe('blocked');
    expect(report.headRev).toBe(6);
    expect(escrituras).toEqual([]);
  });
});

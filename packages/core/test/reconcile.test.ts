/**
 * El ciclo de convergencia (reconcile) — fase 5, aceptación ejecutable: cuando el cursor
 * quedó por debajo del piso de retención del server (`min_rev > since`), el pull NO puede
 * reconstruir el diff incremental: se informa `resync-required` SIN borrar nada — el
 * estado local (outbox incluida) se preserva y lo resuelve el llamador. Jamás "arreglar"
 * el cursor tocando el estado, y jamás aplicar media delta.
 */

import { describe, expect, it } from 'vitest';

import type { ApplyDeps } from '../src/apply-runner.js';
import { pullOnce } from '../src/reconcile.js';
import type { SyncConnection } from '../src/SyncConnection.js';

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

/**
 * Suite de CONTRATO del store: la misma batería corre contra IndexedDB (el plugin) y
 * contra SQLite (el CLI). Si los dos backends no se comportan igual, el sync pierde datos
 * de una forma que sólo se nota cuando ya pasó.
 *
 * Hoy corre contra SQLite; el contrato es el que hace posible que mañana corra contra IDB
 * sin tocar un solo caso.
 */

import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import type {
  BaseEntry,
  IndexEntry,
  IntentEntry,
  OutboxEntry,
  Store,
  TransferEntry,
} from '@ovaultd/core';

import { SqliteStore } from '../src/SqliteStore.js';

const indice = (pathId: string, rev = 1): IndexEntry => ({
  pathId,
  path: `notas/${pathId}.md`,
  kind: 'file',
  deleted: false,
  rev,
});

const base = (pathId: string, rev = 1): BaseEntry => ({ pathId, rev, blobId: `blob-${pathId}` });

const outbox = (operationId: string, queuedAt: number, pathId = 'p1'): OutboxEntry => ({
  operationId,
  kind: 'add',
  pathId,
  expectedRev: 0,
  queuedAt,
  state: 'pending',
});

const transferencia = (transferId: string, updatedAt: number): TransferEntry => ({
  transferId,
  blobId: `blob-${transferId}`,
  direction: 'up',
  received: 0,
  total: 100,
  updatedAt,
});

const intencion = (intentId: string, state: IntentEntry['state'] = 'pending'): IntentEntry => ({
  intentId,
  pathId: `p-${intentId}`,
  rev: 1,
  blobId: `blob-${intentId}`,
  state,
});

describe('contrato del store', () => {
  let dir: string;
  let store: Store;

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'ovaultd-store-'));
    store = SqliteStore.open(join(dir, 'state.db'));
  });

  afterEach(() => {
    store.close();
    rmSync(dir, { recursive: true, force: true });
  });

  // ---------------------------------------------------------------- checkpoint

  it('checkpoint: escribe, lee y NUNCA retrocede', async () => {
    expect(await store.readCheckpoint()).toBeNull();
    await store.writeCheckpoint({ since: 10, minRev: 3 });
    expect((await store.readCheckpoint())?.since).toBe(10);

    // Un ack idempotente con una rev vieja no rebobina el pull.
    const atras = await store.writeCheckpoint({ since: 4, minRev: 3 });
    expect(atras.since).toBe(10);
    expect((await store.readCheckpoint())?.since).toBe(10);

    const adelante = await store.writeCheckpoint({ since: 20, minRev: null });
    expect(adelante.since).toBe(20);
    expect((await store.readCheckpoint())?.minRev).toBe(3);
  });

  // ---------------------------------------------------------------- index / base

  it('index: put, get, delete y list', async () => {
    await store.putIndex(indice('a'));
    await store.putIndex(indice('b', 2));
    expect((await store.getIndex('a'))?.rev).toBe(1);
    expect((await store.listIndex()).map((e) => e.pathId).sort()).toEqual(['a', 'b']);
    await store.deleteIndex('a');
    expect(await store.getIndex('a')).toBeNull();
    expect(await store.listIndex()).toHaveLength(1);
  });

  it('index: put sobre la misma clave reemplaza, no duplica', async () => {
    await store.putIndex(indice('a', 1));
    await store.putIndex(indice('a', 9));
    const todos = await store.listIndex();
    expect(todos).toHaveLength(1);
    expect(todos[0]?.rev).toBe(9);
  });

  it('base: put, get, delete y list', async () => {
    await store.putBase(base('a'));
    expect((await store.getBase('a'))?.blobId).toBe('blob-a');
    expect(await store.listBase()).toHaveLength(1);
    await store.deleteBase('a');
    expect(await store.getBase('a')).toBeNull();
  });

  // ---------------------------------------------------------------- outbox

  it('outbox: FIFO por queuedAt, no por orden de inserción', async () => {
    await store.putOutbox(outbox('tarde', 300));
    await store.putOutbox(outbox('temprano', 100));
    await store.putOutbox(outbox('medio', 200));
    expect((await store.listOutbox()).map((e) => e.operationId)).toEqual([
      'temprano',
      'medio',
      'tarde',
    ]);
  });

  it('outbox: putOutboxMany encola todo', async () => {
    await store.putOutboxMany([outbox('a', 1), outbox('b', 2)]);
    expect(await store.listOutbox()).toHaveLength(2);
  });

  it('outbox: setOutboxState transiciona y falla si no existe', async () => {
    await store.putOutbox(outbox('a', 1));
    const enviada = await store.setOutboxState('a', 'sent');
    expect(enviada.state).toBe('sent');
    await expect(store.setOutboxState('no-existe', 'sent')).rejects.toThrow();
  });

  it('outbox: descartarOutboxDePath marca rejected y devuelve cuántas', async () => {
    await store.putOutbox(outbox('a', 1, 'p1'));
    await store.putOutbox(outbox('b', 2, 'p2'));
    const descartadas = await store.descartarOutboxDePath('p1', 'el pull aplicó la del server');
    expect(descartadas).toBe(1);
    expect((await store.getOutbox('a'))?.state).toBe('rejected');
    expect((await store.getOutbox('a'))?.lastError).toContain('el pull');
    expect((await store.getOutbox('b'))?.state).toBe('pending');
  });

  it('outbox: confirmarLote borra la outbox y avanza checkpoint en una transacción', async () => {
    await store.putOutbox(outbox('a', 1, 'p1'));
    await store.putOutbox(outbox('b', 2, 'p2'));
    await store.confirmarLote(
      [{ entry: outbox('a', 1, 'p1'), base: base('p1'), index: indice('p1') }],
      { since: 7, minRev: null },
    );
    expect(await store.getOutbox('a')).toBeNull();
    expect(await store.getOutbox('b')).not.toBeNull();
    expect((await store.readCheckpoint())?.since).toBe(7);
    expect(await store.getBase('p1')).not.toBeNull();
    expect(await store.getIndex('p1')).not.toBeNull();
  });

  // ---------------------------------------------------------------- transfers

  it('transfers: put, get, list ordenado por updatedAt y delete', async () => {
    await store.putTransfer(transferencia('t2', 200));
    await store.putTransfer(transferencia('t1', 100));
    expect((await store.listTransfers()).map((t) => t.transferId)).toEqual(['t1', 't2']);
    expect((await store.getTransfer('t1'))?.total).toBe(100);
    await store.deleteTransfer('t1');
    expect(await store.getTransfer('t1')).toBeNull();
  });

  // ---------------------------------------------------------------- intents

  it('intents: lista por estado y por todos', async () => {
    await store.putIntent(intencion('a', 'pending'));
    await store.putIntent(intencion('b', 'verified'));
    expect((await store.listIntents('pending')).map((i) => i.intentId)).toEqual(['a']);
    expect(await store.listIntents()).toHaveLength(2);
  });

  it('intents: markIntentVerified cambia el estado y falla si no existe', async () => {
    await store.putIntent(intencion('a'));
    expect((await store.markIntentVerified('a')).state).toBe('verified');
    await expect(store.markIntentVerified('no-existe')).rejects.toThrow();
  });

  it('intents: completeApply cierra la intención y mueve base+índice+checkpoint juntos', async () => {
    await store.putIntent(intencion('a'));
    await store.completeApply({
      intentId: 'a',
      base: base('p-a'),
      index: indice('p-a'),
      checkpoint: { since: 5, minRev: 2 },
    });
    expect(await store.getIntent('a')).toBeNull();
    expect(await store.getBase('p-a')).not.toBeNull();
    expect(await store.getIndex('p-a')).not.toBeNull();
    expect((await store.readCheckpoint())?.since).toBe(5);
  });

  it('intents: completeApply sin intención NO escribe nada (todo o nada)', async () => {
    await store.completeApply({
      intentId: 'fantasma',
      base: base('p-x'),
      index: indice('p-x'),
      checkpoint: { since: 99, minRev: null },
    }).catch(() => undefined);
    expect(await store.getBase('p-x')).toBeNull();
    expect(await store.getIndex('p-x')).toBeNull();
    expect(await store.readCheckpoint()).toBeNull();
  });

  // ---------------------------------------------------------------- durabilidad

  it('los datos sobreviven al cierre y reapertura', async () => {
    await store.putIndex(indice('persistente'));
    await store.writeCheckpoint({ since: 42, minRev: null });
    store.close();

    store = SqliteStore.open(join(dir, 'state.db'));
    expect((await store.getIndex('persistente'))?.pathId).toBe('persistente');
    expect((await store.readCheckpoint())?.since).toBe(42);
  });
});

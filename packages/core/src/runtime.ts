/**
 * Entrada del runtime: este archivo se bundlea a `dist/runtime.js` (IIFE) y se
 * carga DENTRO de la página de Chromium. Expone `globalThis.ovaultdRuntime.dispatch`,
 * el único punto de entrada del puente JSON Node↔runtime.
 *
 * El protocolo del puente es estable desde el día uno:
 *   request : { id: number, method: string, params?: unknown }
 *   response: { id, ok: true, result } | { id, ok: false, error: { message, name? } }
 *
 * Corte 1: runtime + StateStore. Corte 2: outbox save-first (enqueue determinista con
 * snapshot verificado) y journal de aplicación (begin/verify/complete atómico). Los
 * métodos de sync se agregan encima, no al lado.
 */

import { b64Encode } from '@ovaultd/protocol';

import { createIntent, type ApplyCandidate } from './apply.js';
import { prepareOutboxEntry, type EnqueueRequest } from './outbox.js';
import {
  SCHEMA_VERSION,
  STORE_NAMES,
  StateStore,
  type CompleteApplyInput,
  type OutboxEntry,
} from './StateStore.js';

export const RUNTIME_VERSION = '0.2.0';

interface RuntimeRequest {
  id: number;
  method: string;
  params?: Record<string, unknown>;
}

interface RuntimeResponse {
  id: number;
  ok: boolean;
  result?: unknown;
  error?: { message: string; name?: string };
}

let store: StateStore | null = null;

const exigirStore = (): StateStore => {
  if (!store) throw new Error('el StateStore no está abierto (llamá a store.open primero)');
  return store;
};

/** Serializa una entrada de outbox para el wire JSON (los Blob no viajan por el puente). */
const serializarOutbox = (entry: OutboxEntry) => ({
  operationId: entry.operationId,
  kind: entry.kind,
  pathId: entry.pathId,
  path: entry.path ?? null,
  prevPathId: entry.prevPathId ?? null,
  expectedRev: entry.expectedRev,
  queuedAt: entry.queuedAt,
  state: entry.state,
  snapshotHash: entry.snapshotHash ?? null,
  snapshotSize: entry.snapshot ? entry.snapshot.size : null,
});

type Handler = (params: Record<string, unknown>) => Promise<unknown> | unknown;

const handlers: Record<string, Handler> = {
  'runtime.ping': () => 'pong',

  'runtime.info': () => ({
    version: RUNTIME_VERSION,
    schemaVersion: SCHEMA_VERSION,
    origin: globalThis.location.origin,
    userAgent: globalThis.navigator.userAgent,
  }),

  // ------------------------------------------------------------------ state store

  'store.open': async (params) => {
    const name = typeof params.name === 'string' ? params.name : undefined;
    if (store) store.close();
    store = await StateStore.open(name);
    return { schemaVersion: SCHEMA_VERSION, stores: [...STORE_NAMES] };
  },

  'store.close': () => {
    exigirStore().close();
    store = null;
    return { closed: true };
  },

  'store.checkpoint.read': async () => exigirStore().readCheckpoint(),

  'store.checkpoint.write': async (params) => {
    const since = Number(params.since);
    if (!Number.isFinite(since) || since < 0) throw new Error(`since inválido: ${String(params.since)}`);
    const minRev = params.minRev === null || params.minRev === undefined ? null : Number(params.minRev);
    return exigirStore().writeCheckpoint({ since, minRev, updatedAt: Date.now() });
  },

  'store.base.get': (params) => exigirStore().getBase(String(params.pathId)),

  'store.index.get': (params) => exigirStore().getIndex(String(params.pathId)),

  // ------------------------------------------------------------------ outbox

  'outbox.enqueue': async (params) => {
    const request = params.request as EnqueueRequest;
    if (!request || typeof request !== 'object') throw new Error('outbox.enqueue: falta request');
    const entry = prepareOutboxEntry(request);

    // Idempotencia por payload: el mismo payload NO duplica la operación.
    const existente = await exigirStore().getOutbox(entry.operationId);
    if (existente) return { created: false, entry: serializarOutbox(existente) };

    await exigirStore().putOutbox(entry);
    return { created: true, entry: serializarOutbox(entry) };
  },

  'outbox.get': async (params) => {
    const entry = await exigirStore().getOutbox(String(params.operationId));
    return entry ? serializarOutbox(entry) : null;
  },

  /** Devuelve el snapshot recuperable en base64 (verificación byte a byte). */
  'outbox.snapshot': async (params) => {
    const entry = await exigirStore().getOutbox(String(params.operationId));
    if (!entry || !entry.snapshot) return null;
    const bytes = new Uint8Array(await entry.snapshot.arrayBuffer());
    return { b64: b64Encode(bytes), hash: entry.snapshotHash ?? null, size: bytes.length };
  },

  'outbox.list': async () => (await exigirStore().listOutbox()).map(serializarOutbox),

  'outbox.markSent': async (params) =>
    serializarOutbox(await exigirStore().setOutboxState(String(params.operationId), 'sent')),

  'outbox.confirm': async (params) =>
    serializarOutbox(await exigirStore().setOutboxState(String(params.operationId), 'confirmed')),

  'outbox.remove': async (params) => {
    await exigirStore().deleteOutbox(String(params.operationId));
    return { ok: true };
  },

  // Compatibilidad de bajo nivel (el spec del corte 1 la usa): entrada cruda, sin Blob.
  'store.outbox.put': async (params) => {
    const entry = params.entry as Parameters<StateStore['putOutbox']>[0];
    if (!entry || typeof entry.operationId !== 'string') throw new Error('outbox.put: falta operationId');
    await exigirStore().putOutbox(entry);
    return { ok: true };
  },

  'store.outbox.delete': async (params) => {
    await exigirStore().deleteOutbox(String(params.operationId));
    return { ok: true };
  },

  // ------------------------------------------------------------------ journal de aplicación

  'apply.begin': async (params) => {
    const candidate = params.candidate as ApplyCandidate;
    if (!candidate || typeof candidate !== 'object') throw new Error('apply.begin: falta candidate');
    const intent = createIntent(candidate);

    // Repetir el begin de la MISMA revisión no duplica: la intención es determinista.
    const existente = await exigirStore().getIntent(intent.intentId);
    if (existente) return { created: false, intent: existente };

    await exigirStore().putIntent(intent);
    return { created: true, intent };
  },

  'apply.verify': async (params) => exigirStore().markIntentVerified(String(params.intentId)),

  'apply.pending': () => exigirStore().listIntents(),

  'apply.abort': async (params) => {
    await exigirStore().deleteIntent(String(params.intentId));
    return { ok: true };
  },

  'apply.complete': async (params) => {
    const input = params.input as CompleteApplyInput;
    if (!input || typeof input.intentId !== 'string') throw new Error('apply.complete: falta input.intentId');
    await exigirStore().completeApply(input);
    return { ok: true };
  },
};

const dispatch = async (request: RuntimeRequest): Promise<RuntimeResponse> => {
  try {
    const handler = handlers[request.method];
    if (!handler) throw new Error(`método desconocido: ${request.method}`);
    const result = await handler(request.params ?? {});
    return { id: request.id, ok: true, result };
  } catch (error) {
    const err = error instanceof Error ? error : new Error(String(error));
    return { id: request.id, ok: false, error: { message: err.message, name: err.name } };
  }
};

(globalThis as { ovaultdRuntime?: unknown }).ovaultdRuntime = {
  dispatch,
  version: RUNTIME_VERSION,
};

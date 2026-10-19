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
 * snapshot verificado) y journal de aplicación (begin/verify/complete atómico). Corte 3:
 * orquestación real de `apply` contra el filesystem (puente confinado al vault) y
 * recuperación de intenciones al reiniciar. Corte 4: `SyncConnection` (WS+HTTP contra el
 * wire, commit solo por WS) y `pullOnce` (convergencia). Corte 5: `pushOnce` (save-first →
 * cifrar → firmar → batch por WS) y `correrCiclo` — la tabla de decisión pull→push con
 * conflictos explícitos (SyncClient).
 */

import type { SubKeys } from '@ovaultd/crypto';
import { b64Decode, b64Encode, fromHex } from '@ovaultd/protocol';

import { applyRevision, recoverApplies, type ApplyRevision } from './apply-runner.js';
import { createIntent, type ApplyCandidate } from './apply.js';
import { HttpFileSystemPort } from './file-port.js';
import { contenidoDeRevision, diffDeTextos, historiaDeArchivo } from './history.js';
import { prepareOutboxEntry, type EnqueueRequest } from './outbox.js';
import { pushOnce } from './push.js';
import { pullOnce } from './reconcile.js';
import { diffLocal, escanearVault } from './scan.js';
import { correrCiclo } from './SyncClient.js';
import { SyncConnection } from './SyncConnection.js';
import {
  SCHEMA_VERSION,
  STORE_NAMES,
  StateStore,
  type CompleteApplyInput,
  type OutboxEntry,
} from './StateStore.js';

export const RUNTIME_VERSION = '0.6.0';

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

/** Subclaves del vault configuradas para aplicar (apply.configure). */
let applyKeys: SubKeys | null = null;

/** Nombre del dispositivo para el sello de las copias de conflicto (apply.configure). */
let applyDeviceName: string | null = null;

const exigirApply = (): { store: StateStore; keys: SubKeys } => {
  if (!applyKeys) throw new Error('apply: falta apply.configure (subclaves del vault)');
  return { store: exigirStore(), keys: applyKeys };
};

/** Conexión de sync abierta (sync.open), reutilizada por sync.pullOnce. */
let syncConn: SyncConnection | null = null;

/** Material de firma del vault para el push (`sync.open` lo recibe del estado local). */
let syncVaultMeta: { vaultUuid: Uint8Array; epoch: number } | null = null;

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

  'apply.verify': async (params) => {
    const intentId = String(params.intentId);
    const intent = await exigirStore().getIntent(intentId);
    if (!intent) throw new Error(`apply: no existe la intención ${intentId}`);
    const tempName = typeof params.tempName === 'string' ? params.tempName : intent.tempName;
    const updated = { ...intent, tempName, state: 'verified' as const };
    await exigirStore().putIntent(updated);
    return updated;
  },

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

  // ------------------------------------------------------------------ apply (orquestación real)

  'apply.configure': (params) => {
    const raw = params.keys as Record<string, unknown> | undefined;
    const subclaves = ['content', 'nonce', 'path', 'meta', 'bind', 'sign'] as const;
    const keys = {} as Record<(typeof subclaves)[number], Uint8Array>;
    for (const nombre of subclaves) {
      const valor = raw?.[nombre];
      if (typeof valor !== 'string' || !/^[0-9a-fA-F]{64}$/.test(valor)) {
        throw new Error(`apply.configure: falta la subclave ${nombre} (hex de 32 B)`);
      }
      keys[nombre] = fromHex(valor);
    }
    applyKeys = keys as unknown as SubKeys;
    if (typeof params.deviceName === 'string' && params.deviceName.trim().length > 0) {
      applyDeviceName = params.deviceName.trim().slice(0, 64);
    }
    return { configured: true, subclaves: subclaves.length };
  },

  'apply.run': async (params) => {
    const { store, keys } = exigirApply();
    const revision = params.revision as ApplyRevision;
    if (!revision || typeof revision !== 'object') throw new Error('apply.run: falta revision');
    const stored = typeof params.storedB64 === 'string' ? b64Decode(params.storedB64) : null;
    return applyRevision(
      {
        store,
        port: new HttpFileSystemPort(globalThis.location.origin),
        keys,
        deviceName: applyDeviceName ?? undefined,
      },
      revision,
      stored,
    );
  },

  'apply.recover': async () => {
    const { store, keys } = exigirApply();
    return recoverApplies({
      store,
      port: new HttpFileSystemPort(globalThis.location.origin),
      keys,
      deviceName: applyDeviceName ?? undefined,
    });
  },

  // ------------------------------------------------------------------ sync (corte 4)

  'sync.open': async (params) => {
    const { keys } = exigirApply();
    if (syncConn) {
      syncConn.close();
      syncConn = null;
    }
    const vaultUuid = typeof params.vaultUuid === 'string' ? b64Decode(params.vaultUuid) : null;
    if (!vaultUuid || vaultUuid.length !== 16) {
      throw new Error('sync.open: falta vaultUuid (16 B en base64, el del material del vault)');
    }
    const conn = new SyncConnection({
      baseUrl: String(params.baseUrl),
      token: String(params.token),
      vault: Number(params.vault),
      device: Number(params.device),
      keys,
      // El HTTP del cliente pasa por el PUENTE (mismo origen que la página): el cache del
      // preflight CORS del navegador es por URL y cada blob tiene la suya, así que un PUT
      // directo pagaba un OPTIONS de ~40 ms por blob. El puente lo reenvía con node.
      fetchImpl: async (input, init) => {
        const peticion = input instanceof Request ? input : null;
        const url = peticion ? peticion.url : String(input);
        const metodo = init?.method ?? peticion?.method ?? 'GET';
        const headers = (init?.headers ?? {}) as Record<string, string>;
        const cuerpo = init?.body as ArrayBuffer | undefined;
        // Binario CRUDO (sin base64 ni JSON grande): el sobre de un blob de 100 KB son
        // ~133 KB y codificarlo en la página costaba 40-100 ms por PUT. Los metadatos van
        // en headers y el body viaja tal cual; el puente lo reenvía con node.
        const res = await fetch('/fs/proxyFetch', {
          method: 'POST',
          headers: {
            'content-type': 'application/octet-stream',
            'x-ovaultd-url': encodeURIComponent(url),
            'x-ovaultd-method': metodo,
            'x-ovaultd-headers': JSON.stringify(headers),
          },
          body: cuerpo ?? null,
        });
        if (!res.ok) throw new Error(`puente proxyFetch: HTTP ${res.status}`);
        const status = Number(res.headers.get('x-ovaultd-status') ?? 502);
        const bytes = await res.arrayBuffer();
        return new Response(bytes.byteLength ? bytes : null, { status });
      },
    });
    const { headRev } = await conn.open({ since: Number(params.since ?? 0) });
    syncConn = conn;
    syncVaultMeta = { vaultUuid, epoch: Number(params.epoch ?? 1) };
    return { headRev };
  },

  'sync.pullOnce': async () => {
    if (!syncConn) throw new Error('sync: no hay conexión abierta (sync.open primero)');
    const { store, keys } = exigirApply();
    return pullOnce(syncConn, {
      store,
      port: new HttpFileSystemPort(globalThis.location.origin),
      keys,
      deviceName: applyDeviceName ?? undefined,
    });
  },

  /** Escaneo + hash-diff sin tocar la red (lo que muestra `list-local`). */
  'sync.scan': async () => {
    const { store, keys } = exigirApply();
    const port = new HttpFileSystemPort(globalThis.location.origin);
    const entradas = await escanearVault(port, keys);
    const diff = await diffLocal(store, entradas);
    return {
      diff,
      entradas: entradas.map((entrada) => ({
        path: entrada.path,
        pathId: entrada.pathId,
        size: entrada.size,
        contentHash: entrada.contentHash,
      })),
    };
  },

  'sync.pushOnce': async () => {
    if (!syncConn || !syncVaultMeta) {
      throw new Error('sync: no hay conexión abierta (sync.open primero)');
    }
    const { store, keys } = exigirApply();
    return pushOnce({
      store,
      port: new HttpFileSystemPort(globalThis.location.origin),
      keys,
      conn: syncConn,
      vaultUuid: syncVaultMeta.vaultUuid,
      epoch: syncVaultMeta.epoch,
    });
  },

  // -------------------------------------------------------------- history (corte 5b)

  /**
   * Historia de un archivo (LECTURA pura): usa la conexión abierta SOLO para leer.
   * No toca el estado de sync ni el vault.
   */
  'history.list': async (params) => {
    if (!syncConn) throw new Error('history: no hay conexión abierta (sync.open primero)');
    const { keys } = exigirApply();
    const ruta = typeof params.ruta === 'string' ? params.ruta : '';
    if (!ruta) throw new Error('history.list: falta ruta');
    return historiaDeArchivo({
      conn: syncConn,
      keys,
      ruta,
      before:
        params.before === undefined || params.before === null ? undefined : Number(params.before),
      limit: params.limit === undefined ? undefined : Number(params.limit),
    });
  },

  /** Contenido descifrado de una revisión (verifica el hash del blob antes de descifrar). */
  'history.preview': async (params) => {
    if (!syncConn) throw new Error('history: no hay conexión abierta (sync.open primero)');
    const { keys } = exigirApply();
    const blobId = typeof params.blobId === 'string' ? params.blobId : '';
    if (!blobId) throw new Error('history.preview: falta blobId');
    const contenido = await contenidoDeRevision({ conn: syncConn, keys, blobId });
    // El puente es JSON: el texto viaja como string. Un binario no puede pasar por
    // acá (el preview es para notas): mejor un error claro que mojibake.
    let texto: string;
    try {
      texto = new TextDecoder('utf-8', { fatal: true }).decode(contenido);
    } catch {
      throw new Error(`la revisión no es texto UTF-8 (${contenido.length} bytes binarios)`);
    }
    return { texto, bytes: contenido.length };
  },

  /** Diff legible por líneas entre dos textos (lo usa el preview del CLI). */
  'history.diff': (params) => {
    const anterior = typeof params.anterior === 'string' ? params.anterior : '';
    const nuevo = typeof params.nuevo === 'string' ? params.nuevo : '';
    return { diff: diffDeTextos(anterior, nuevo) };
  },

  'sync.close': () => {
    syncConn?.close();
    syncConn = null;
    syncVaultMeta = null;
    return { closed: true };
  },

  /**
   * Ciclo completo (SyncClient): pull → push con la tabla de decisión. Pull bloqueado
   * o resync ⇒ NO se pushea (el checkpoint quedaría atrás de una revisión sin aplicar).
   */
  'sync.cycle': async () => {
    if (!syncConn || !syncVaultMeta) {
      throw new Error('sync: no hay conexión abierta (sync.open primero)');
    }
    const { store, keys } = exigirApply();
    return correrCiclo({
      store,
      port: new HttpFileSystemPort(globalThis.location.origin),
      keys,
      conn: syncConn,
      vaultUuid: syncVaultMeta.vaultUuid,
      epoch: syncVaultMeta.epoch,
    });
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

/**
 * `StateStore` — el estado durable del cliente, en IndexedDB.
 *
 * Es la ÚNICA fuente de verdad local (decisión del plan: nada de SQLite local, nada de
 * `fake-indexeddb` en producción). Stores versionados:
 *
 *  - `index`      → qué conoce el cliente de cada path (entrada actual + metadatos).
 *  - `base`       → la revisión BASE sincronizada por path (para 3-vías y compare-and-swap).
 *  - `outbox`     → operaciones locales pendientes, con su `operation_id` y snapshot.
 *  - `transfers`  → transferencias parciales (subida/bajada reanudable).
 *  - `intents`    → intenciones de aplicación (journal IndexedDB↔filesystem).
 *  - `checkpoint` → el cursor `since`/`min_rev` aplicado recuperablemente.
 *
 * Regla dura del núcleo: `since` avanza SOLO cuando la revisión quedó recuperablemente
 * aplicada. Este archivo no conoce el wire: es estado puro.
 */

import { openDB, type DBSchema, type IDBPDatabase } from 'idb';

import { obsoletasDePath } from './outbox.js';

export const SCHEMA_VERSION = 1;
export const DB_NAME_DEFAULT = 'ovaultd';

/** Entrada del índice: lo que el cliente sabe de un path. */
export interface IndexEntry {
  /** `path_id` en hex (clave). */
  pathId: string;
  /** Path descifrado, si se conoce. */
  path?: string;
  kind: 'file' | 'folder';
  deleted: boolean;
  /** Última revisión conocida de la entrada. */
  rev: number;
  /** `blob_id` en hex de la revisión vigente (null en carpetas/tombstones). */
  blobId?: string | null;
  size?: number | null;
  mtime?: number | null;
  /** Hash del contenido LOCAL (hash-diff), si se computó. */
  contentHash?: string | null;
}

/** Base sincronizada: contra qué revisión se comparan los cambios. */
export interface BaseEntry {
  /** `path_id` en hex (clave). */
  pathId: string;
  rev: number;
  blobId?: string | null;
  contentHash?: string | null;
}

/** Operación pendiente de publicación. */
export interface OutboxEntry {
  /** `operation_id` (UUID textual): se genera UNA vez y sobrevive reintentos. */
  operationId: string;
  kind: 'add' | 'mod' | 'del' | 'move';
  pathId: string;
  path?: string;
  prevPathId?: string;
  /** Revisión contra la que se hará compare-and-swap. */
  expectedRev: number;
  queuedAt: number;
  /** Snapshot recuperable del contenido local (save-first). */
  snapshot?: Blob | null;
  snapshotHash?: string | null;
  /** Estado de publicación: pending → sent (ack) → confirmed; `rejected` = el server la
   *  rechazó por un problema de ESTA operación (archivo demasiado grande, sin snapshot,
   *  bad_request) y no se reintenta sola: queda visible para el usuario. */
  state: 'pending' | 'sent' | 'confirmed' | 'rejected';
  /** Motivo del rechazo (sólo en `rejected`). */
  lastError?: string | null;
  /** mtime del archivo al encolar: alimenta el cache del escaneo incremental. */
  mtimeMs?: number | null;
}

/** Transferencia parcial (reanudable). */
export interface TransferEntry {
  transferId: string;
  blobId: string;
  direction: 'up' | 'down';
  received: number;
  total: number;
  updatedAt: number;
  part?: Blob | null;
}

/** Estado remoto de una fusión aplicada (ver `IntentEntry.mergeBase`). */
export interface IntentMergeBase {
  rev: number;
  blobId: string | null;
  contentHash: string | null;
  oursHash: string;
}

/** Intención de aplicación (journal IndexedDB↔filesystem). */
export interface IntentEntry {
  intentId: string;
  pathId: string;
  rev: number;
  blobId: string;
  /** pending → verified → (commit) | blocked (tercera versión: espera resolución). */
  state: 'pending' | 'verified' | 'blocked';
  /** Ruta relativa del destino en el vault (texto en claro, para la recuperación). */
  path?: string;
  /** La operación era un borrado. */
  deleted?: boolean;
  /** Tipo de entrada. */
  kind?: 'file' | 'folder';
  /** sha256 hex del PLAINTEXT esperado: permite recuperar sin el server. */
  contentHash?: string | null;
  /**
   * Fusión 3-vías aplicada (el intent nació de un merge automático): guarda el estado
   * REMOTO con el que quedó sincronizado el path y el hash del local que se fusionó.
   * El commit escribe base/índice con el `contentHash` REMOTO (no el de la fusión): así
   * el scan ve que el archivo difiere del índice y el push sube la fusión como revisión
   * nueva. `oursHash` es el control del recovery: si el destino ya no es ni el local
   * que se fusionó, se bloquea en vez de pisar.
   */
  mergeBase?: IntentMergeBase | null;
  /** Nombre del archivo temporal en el vault (relativo). */
  tempName?: string;
  /** Por qué quedó bloqueada (si state === 'blocked'). */
  reason?: string;
  createdAt: number;
}

/** Checkpoint: el cursor aplicado recuperablemente. */
export interface CheckpointEntry {
  id: 'checkpoint';
  since: number;
  minRev: number | null;
  updatedAt: number;
}

interface OVaultdDB extends DBSchema {
  index: { key: string; value: IndexEntry };
  base: { key: string; value: BaseEntry };
  outbox: { key: string; value: OutboxEntry; indexes: { by_queued: number } };
  transfers: { key: string; value: TransferEntry; indexes: { by_updated: number } };
  intents: { key: string; value: IntentEntry; indexes: { by_state: string } };
  checkpoint: { key: string; value: CheckpointEntry };
}

type StoreName = 'index' | 'base' | 'outbox' | 'transfers' | 'intents' | 'checkpoint';

export const STORE_NAMES: readonly StoreName[] = [
  'index',
  'base',
  'outbox',
  'transfers',
  'intents',
  'checkpoint',
];

/**
 * Migraciones versionadas: el índice ES la versión (MIGRATIONS[i] lleva la DB de
 * versión i a i+1). Las bases reales de usuarios se migran acá; nunca se recrean.
 */
const MIGRATIONS: Array<(db: IDBPDatabase<OVaultdDB>) => void> = [
  // v0 → v1: esquema inicial completo.
  (db) => {
    db.createObjectStore('index', { keyPath: 'pathId' });
    db.createObjectStore('base', { keyPath: 'pathId' });
    const outbox = db.createObjectStore('outbox', { keyPath: 'operationId' });
    outbox.createIndex('by_queued', 'queuedAt');
    const transfers = db.createObjectStore('transfers', { keyPath: 'transferId' });
    transfers.createIndex('by_updated', 'updatedAt');
    const intents = db.createObjectStore('intents', { keyPath: 'intentId' });
    intents.createIndex('by_state', 'state');
    db.createObjectStore('checkpoint', { keyPath: 'id' });
  },
];

export class StateStore {
  private constructor(private readonly db: IDBPDatabase<OVaultdDB>) {}

  static async open(name: string = DB_NAME_DEFAULT): Promise<StateStore> {
    const db = await openDB<OVaultdDB>(name, SCHEMA_VERSION, {
      upgrade(database, oldVersion) {
        for (let version = oldVersion; version < SCHEMA_VERSION; version += 1) {
          MIGRATIONS[version]?.(database);
        }
      },
      blocked() {
        // Otra pestaña/runtime con una versión vieja tiene la DB abierta: no la pisamos.
        console.warn('[ovaultd] IndexedDB bloqueada por una conexión anterior');
      },
    });
    return new StateStore(db);
  }

  /** Cierra la conexión (los datos quedan durables en el perfil). */
  close(): void {
    this.db.close();
  }

  // ------------------------------------------------------------------ checkpoint

  async readCheckpoint(): Promise<CheckpointEntry | null> {
    return (await this.db.get('checkpoint', 'checkpoint')) ?? null;
  }

  async writeCheckpoint(
    entry: Omit<CheckpointEntry, 'id' | 'updatedAt'> & { updatedAt?: number },
  ): Promise<CheckpointEntry> {
    // El cursor NUNCA retrocede: un ack idempotente con una rev vieja (o resolver un intent
    // de una rev anterior) rebobinaba el pull y re-aplicaba revisiones ya aplicadas.
    const actual = await this.db.get('checkpoint', 'checkpoint');
    const value: CheckpointEntry = {
      id: 'checkpoint',
      since: actual ? Math.max(actual.since, entry.since) : entry.since,
      minRev: entry.minRev ?? actual?.minRev ?? null,
      updatedAt: entry.updatedAt ?? Date.now(),
    };
    await this.db.put('checkpoint', value);
    return value;
  }

  // ------------------------------------------------------------------ index

  async getIndex(pathId: string): Promise<IndexEntry | null> {
    return (await this.db.get('index', pathId)) ?? null;
  }

  async putIndex(entry: IndexEntry): Promise<void> {
    await this.db.put('index', entry);
  }

  async deleteIndex(pathId: string): Promise<void> {
    await this.db.delete('index', pathId);
  }

  async listIndex(): Promise<IndexEntry[]> {
    return this.db.getAll('index');
  }

  // ------------------------------------------------------------------ base

  async getBase(pathId: string): Promise<BaseEntry | null> {
    return (await this.db.get('base', pathId)) ?? null;
  }

  async putBase(entry: BaseEntry): Promise<void> {
    await this.db.put('base', entry);
  }

  async deleteBase(pathId: string): Promise<void> {
    await this.db.delete('base', pathId);
  }

  async listBase(): Promise<BaseEntry[]> {
    return this.db.getAll('base');
  }

  // ------------------------------------------------------------------ outbox

  async putOutbox(entry: OutboxEntry): Promise<void> {
    await this.db.put('outbox', entry);
  }

  /** Encola varias entradas en UNA transacción (el push inicial encola cientos de golpe). */
  async putOutboxMany(entries: OutboxEntry[]): Promise<void> {
    if (entries.length === 0) return;
    const tx = this.db.transaction('outbox', 'readwrite');
    const store = tx.objectStore('outbox');
    for (const entry of entries) await store.put(entry);
    await tx.done;
  }

  async getOutbox(operationId: string): Promise<OutboxEntry | null> {
    return (await this.db.get('outbox', operationId)) ?? null;
  }

  /** Outbox en orden FIFO de encolado (cola serial). */
  async listOutbox(): Promise<OutboxEntry[]> {
    return this.db.getAllFromIndex('outbox', 'by_queued');
  }

  async deleteOutbox(operationId: string): Promise<void> {
    await this.db.delete('outbox', operationId);
  }

  /** Transición de estado de una operación (pending → sent → confirmed). */
  /**
   * Marca como `rejected` las entradas pendientes de un path: el pull aplicó la versión
   * del server y publicar el snapshot local viejo pisaría esa revisión (el caso que hacía
   * que un conflicto "resuelto" volviera atrás en el ciclo siguiente).
   */
  async descartarOutboxDePath(pathId: string, motivo: string): Promise<number> {
    const entries = await this.db.getAll('outbox');
    const obsoletas = obsoletasDePath(entries, pathId);
    if (obsoletas.length === 0) return 0;
    const tx = this.db.transaction('outbox', 'readwrite');
    for (const entry of obsoletas) {
      await tx.store.put({ ...entry, state: 'rejected', lastError: motivo });
    }
    await tx.done;
    return obsoletas.length;
  }

  async setOutboxState(
    operationId: string,
    state: OutboxEntry['state'],
    lastError?: string | null,
  ): Promise<OutboxEntry> {
    const entry = await this.db.get('outbox', operationId);
    if (!entry) throw new Error(`outbox: no existe la operación ${operationId}`);
    const updated: OutboxEntry = { ...entry, state, lastError: lastError ?? null };
    await this.db.put('outbox', updated);
    return updated;
  }

  /**
   * Confirmación de un LOTE del push en UNA transacción de IndexedDB: base + índice +
   * outbox (borrado) + checkpoint. Antes eran 4 transacciones POR NOTA (~16,5 ms cada
   * una: el piso medido del T1); acá el lote entero paga el costo fijo UNA vez.
   * Es atómica: si el proceso muere, o se confirma el lote entero o nada (las entradas
   * siguen en `sent` y el reintento es idempotente).
   */
  async confirmarLote(
    items: Array<{ entry: OutboxEntry; base: BaseEntry; index: IndexEntry }>,
    checkpoint: { since: number; minRev: number | null },
  ): Promise<void> {
    const tx = this.db.transaction(['base', 'index', 'outbox', 'checkpoint'], 'readwrite');
    const base = tx.objectStore('base');
    const index = tx.objectStore('index');
    const outbox = tx.objectStore('outbox');
    for (const item of items) {
      await base.put(item.base);
      await index.put(item.index);
      await outbox.delete(item.entry.operationId);
    }
    await tx.objectStore('checkpoint').put({
      id: 'checkpoint',
      since: checkpoint.since,
      minRev: checkpoint.minRev,
      updatedAt: Date.now(),
    });
    await tx.done;
  }

  // ------------------------------------------------------------------ transfers

  async putTransfer(entry: TransferEntry): Promise<void> {
    await this.db.put('transfers', entry);
  }

  async getTransfer(transferId: string): Promise<TransferEntry | null> {
    return (await this.db.get('transfers', transferId)) ?? null;
  }

  async listTransfers(): Promise<TransferEntry[]> {
    return this.db.getAllFromIndex('transfers', 'by_updated');
  }

  async deleteTransfer(transferId: string): Promise<void> {
    await this.db.delete('transfers', transferId);
  }

  // ------------------------------------------------------------------ intents

  async putIntent(entry: IntentEntry): Promise<void> {
    await this.db.put('intents', entry);
  }

  async getIntent(intentId: string): Promise<IntentEntry | null> {
    return (await this.db.get('intents', intentId)) ?? null;
  }

  /** Intenciones en un estado (recuperación al reiniciar: se listan las `pending`). */
  async listIntents(state?: IntentEntry['state']): Promise<IntentEntry[]> {
    if (state === undefined) return this.db.getAll('intents');
    return this.db.getAllFromIndex('intents', 'by_state', state);
  }

  async deleteIntent(intentId: string): Promise<void> {
    await this.db.delete('intents', intentId);
  }

  /** Intención → `verified` (hash/AEAD verificados, lista para publicar). */
  async markIntentVerified(intentId: string): Promise<IntentEntry> {
    const intent = await this.db.get('intents', intentId);
    if (!intent) throw new Error(`apply: no existe la intención ${intentId}`);
    const updated: IntentEntry = { ...intent, state: 'verified' };
    await this.db.put('intents', updated);
    return updated;
  }

  /**
   * Commit ATÓMICO del journal: cierra la intención y avanza base/índice/checkpoint en
   * UNA transacción de IndexedDB. `since` avanza SOLO por acá — recibir un frame no
   * equivale a aplicarlo, y un commit a medias no existe.
   */
  async completeApply(input: CompleteApplyInput): Promise<void> {
    const intent = await this.db.get('intents', input.intentId);
    if (!intent) throw new Error(`apply: no existe la intención ${input.intentId}`);

    const tx = this.db.transaction(['intents', 'base', 'index', 'checkpoint'], 'readwrite');
    await tx.objectStore('intents').delete(input.intentId);
    if (input.base) await tx.objectStore('base').put(input.base);
    if (input.index) await tx.objectStore('index').put(input.index);
    if (input.checkpoint) {
      await tx.objectStore('checkpoint').put({
        id: 'checkpoint',
        since: input.checkpoint.since,
        minRev: input.checkpoint.minRev,
        updatedAt: Date.now(),
      });
    }
    await tx.done;
  }
}

/** Entrada del commit atómico del journal. */
export interface CompleteApplyInput {
  intentId: string;
  base?: BaseEntry | null;
  index?: IndexEntry | null;
  checkpoint?: { since: number; minRev: number | null } | null;
}

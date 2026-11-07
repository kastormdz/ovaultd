/**
 * `Store` sobre SQLite para el CLI: el sync sin navegador.
 *
 * El plugin corre dentro de Obsidian y usa IndexedDB (tiene sentido: ya hay un renderer).
 * El CLI no: levantar un Chromium entero (~278 MB) sólo para tener IndexedDB es caro y
 * frágil. Esta implementación usa `node:sqlite` (nativo, sin dependencias) y cumple el
 * mismo contrato (`Store`).
 *
 * Modelo: una tabla `kv(store, key, value)` con el `value` en JSON. Simple a propósito —
 * el estado de un vault son unos pocos miles de filas y lo que importa es que la
 * semántica sea EXACTAMENTE la del otro backend, no la performance. Las transacciones
 * (`confirmarLote`, `completeApply`, `putOutboxMany`, `descartarOutboxDePath`) van con
 * `BEGIN IMMEDIATE`, igual que el commit del server.
 */

import { DatabaseSync } from 'node:sqlite';

import { obsoletasDePath } from '@ovaultd/core';
import type { Store, CompleteApplyInputContrato } from '@ovaultd/core';
import type {
  BaseEntry,
  CheckpointEntry,
  IndexEntry,
  IntentEntry,
  OutboxEntry,
  TransferEntry,
} from '@ovaultd/core';

const ESQUEMA = `
  CREATE TABLE IF NOT EXISTS kv (
    store TEXT NOT NULL,
    key   TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (store, key)
  ) WITHOUT ROWID;
`;

export class SqliteStore implements Store {
  private constructor(private readonly db: DatabaseSync) {}

  static open(ruta: string): SqliteStore {
    const db = new DatabaseSync(ruta);
    // WAL: lecturas concurrentes mientras se escribe, y durable sin fsync por commit.
    db.exec('PRAGMA journal_mode = WAL;');
    db.exec('PRAGMA synchronous = NORMAL;');
    db.exec(ESQUEMA);
    return new SqliteStore(db);
  }

  close(): void {
    this.db.close();
  }

  // ------------------------------------------------------------------ genéricos

  private leer<T>(store: string, key: string): T | null {
    const fila = this.db
      .prepare('SELECT value FROM kv WHERE store = ? AND key = ?')
      .get(store, key) as { value: string } | undefined;
    return fila ? (JSON.parse(fila.value) as T) : null;
  }

  private escribir(store: string, key: string, value: unknown): void {
    this.db
      .prepare(
        `INSERT INTO kv (store, key, value) VALUES (?, ?, ?)
         ON CONFLICT(store, key) DO UPDATE SET value = excluded.value`,
      )
      .run(store, key, JSON.stringify(value));
  }

  private borrar(store: string, key: string): void {
    this.db.prepare('DELETE FROM kv WHERE store = ? AND key = ?').run(store, key);
  }

  private todos<T>(store: string): T[] {
    const filas = this.db
      .prepare('SELECT value FROM kv WHERE store = ?')
      .all(store) as Array<{ value: string }>;
    return filas.map((fila) => JSON.parse(fila.value) as T);
  }

  private enTransaccion<T>(fn: () => T): T {
    this.db.exec('BEGIN IMMEDIATE');
    try {
      const salida = fn();
      this.db.exec('COMMIT');
      return salida;
    } catch (error) {
      this.db.exec('ROLLBACK');
      throw error;
    }
  }

  // ------------------------------------------------------------------ checkpoint

  async readCheckpoint(): Promise<CheckpointEntry | null> {
    return this.leer<CheckpointEntry>('checkpoint', 'checkpoint');
  }

  async writeCheckpoint(
    entry: Omit<CheckpointEntry, 'id' | 'updatedAt'> & { updatedAt?: number },
  ): Promise<CheckpointEntry> {
    // Mismo invariante que el store de IDB: el cursor NUNCA retrocede.
    const actual = this.leer<CheckpointEntry>('checkpoint', 'checkpoint');
    const value: CheckpointEntry = {
      id: 'checkpoint',
      since: actual ? Math.max(actual.since, entry.since) : entry.since,
      minRev: entry.minRev ?? actual?.minRev ?? null,
      updatedAt: entry.updatedAt ?? Date.now(),
    };
    this.escribir('checkpoint', 'checkpoint', value);
    return value;
  }

  // ------------------------------------------------------------------ index

  async getIndex(pathId: string): Promise<IndexEntry | null> {
    return this.leer<IndexEntry>('index', pathId);
  }

  async putIndex(entry: IndexEntry): Promise<void> {
    this.escribir('index', entry.pathId, entry);
  }

  async deleteIndex(pathId: string): Promise<void> {
    this.borrar('index', pathId);
  }

  async listIndex(): Promise<IndexEntry[]> {
    return this.todos<IndexEntry>('index');
  }

  // ------------------------------------------------------------------ base

  async getBase(pathId: string): Promise<BaseEntry | null> {
    return this.leer<BaseEntry>('base', pathId);
  }

  async putBase(entry: BaseEntry): Promise<void> {
    this.escribir('base', entry.pathId, entry);
  }

  async deleteBase(pathId: string): Promise<void> {
    this.borrar('base', pathId);
  }

  async listBase(): Promise<BaseEntry[]> {
    return this.todos<BaseEntry>('base');
  }

  // ------------------------------------------------------------------ outbox

  async putOutbox(entry: OutboxEntry): Promise<void> {
    this.escribir('outbox', entry.operationId, entry);
  }

  async putOutboxMany(entries: OutboxEntry[]): Promise<void> {
    if (entries.length === 0) return;
    this.enTransaccion(() => {
      for (const entry of entries) this.escribir('outbox', entry.operationId, entry);
    });
  }

  async getOutbox(operationId: string): Promise<OutboxEntry | null> {
    return this.leer<OutboxEntry>('outbox', operationId);
  }

  /** FIFO por `queuedAt` (el orden de la cola serial). */
  async listOutbox(): Promise<OutboxEntry[]> {
    return this.todos<OutboxEntry>('outbox').sort((a, b) => a.queuedAt - b.queuedAt);
  }

  async deleteOutbox(operationId: string): Promise<void> {
    this.borrar('outbox', operationId);
  }

  async descartarOutboxDePath(pathId: string, motivo: string): Promise<number> {
    const entries = this.todos<OutboxEntry>('outbox');
    const obsoletas = obsoletasDePath(entries, pathId);
    if (obsoletas.length === 0) return 0;
    this.enTransaccion(() => {
      for (const entry of obsoletas) {
        this.escribir('outbox', entry.operationId, {
          ...entry,
          state: 'rejected',
          lastError: motivo,
        });
      }
    });
    return obsoletas.length;
  }

  async setOutboxState(
    operationId: string,
    state: OutboxEntry['state'],
    lastError?: string | null,
  ): Promise<OutboxEntry> {
    const entry = this.leer<OutboxEntry>('outbox', operationId);
    if (!entry) throw new Error(`outbox: no existe la operación ${operationId}`);
    const updated: OutboxEntry = { ...entry, state, lastError: lastError ?? null };
    this.escribir('outbox', operationId, updated);
    return updated;
  }

  async confirmarLote(
    items: Array<{ entry: OutboxEntry; base: BaseEntry; index: IndexEntry }>,
    checkpoint: { since: number; minRev: number | null },
  ): Promise<void> {
    this.enTransaccion(() => {
      for (const item of items) {
        this.escribir('base', item.base.pathId, item.base);
        this.escribir('index', item.index.pathId, item.index);
        this.borrar('outbox', item.entry.operationId);
      }
      this.escribir('checkpoint', 'checkpoint', {
        id: 'checkpoint',
        since: checkpoint.since,
        minRev: checkpoint.minRev,
        updatedAt: Date.now(),
      } satisfies CheckpointEntry);
    });
  }

  // ------------------------------------------------------------------ transfers

  async putTransfer(entry: TransferEntry): Promise<void> {
    this.escribir('transfers', entry.transferId, entry);
  }

  async getTransfer(transferId: string): Promise<TransferEntry | null> {
    return this.leer<TransferEntry>('transfers', transferId);
  }

  async listTransfers(): Promise<TransferEntry[]> {
    return this.todos<TransferEntry>('transfers').sort((a, b) => a.updatedAt - b.updatedAt);
  }

  async deleteTransfer(transferId: string): Promise<void> {
    this.borrar('transfers', transferId);
  }

  // ------------------------------------------------------------------ intents

  async putIntent(entry: IntentEntry): Promise<void> {
    this.escribir('intents', entry.intentId, entry);
  }

  async getIntent(intentId: string): Promise<IntentEntry | null> {
    return this.leer<IntentEntry>('intents', intentId);
  }

  async listIntents(state?: IntentEntry['state']): Promise<IntentEntry[]> {
    const todos = this.todos<IntentEntry>('intents');
    return state === undefined ? todos : todos.filter((intent) => intent.state === state);
  }

  async deleteIntent(intentId: string): Promise<void> {
    this.borrar('intents', intentId);
  }

  async markIntentVerified(intentId: string): Promise<IntentEntry> {
    const intent = this.leer<IntentEntry>('intents', intentId);
    if (!intent) throw new Error(`apply: no existe la intención ${intentId}`);
    const updated: IntentEntry = { ...intent, state: 'verified' };
    this.escribir('intents', intentId, updated);
    return updated;
  }

  async completeApply(input: CompleteApplyInputContrato): Promise<void> {
    const intent = this.leer<IntentEntry>('intents', input.intentId);
    if (!intent) throw new Error(`apply: no existe la intención ${input.intentId}`);
    this.enTransaccion(() => {
      this.borrar('intents', input.intentId);
      if (input.base) this.escribir('base', input.base.pathId, input.base);
      if (input.index) this.escribir('index', input.index.pathId, input.index);
      if (input.checkpoint) {
        this.escribir('checkpoint', 'checkpoint', {
          id: 'checkpoint',
          since: input.checkpoint.since,
          minRev: input.checkpoint.minRev,
          updatedAt: Date.now(),
        } satisfies CheckpointEntry);
      }
    });
  }
}

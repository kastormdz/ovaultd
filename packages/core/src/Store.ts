/**
 * Contrato del estado local del sync.
 *
 * Existe porque hay DOS backends con la misma semántica: IndexedDB (el plugin, dentro del
 * renderer de Obsidian) y SQLite (el CLI, sin navegador). Sin un contrato explícito, los
 * dos se desincronizan y el sync empieza a perder datos de una forma que sólo se nota
 * cuando ya pasó.
 *
 * `StateStore` (IDB) y `SqliteStore` implementan esta interfaz. La suite de contrato
 * (`test/store-contract.test.ts`) corre contra los dos.
 */

import type { BaseEntry, CheckpointEntry, IndexEntry, IntentEntry, OutboxEntry, TransferEntry } from './StateStore.js';

export interface CompleteApplyInputContrato {
  intentId: string;
  base?: BaseEntry | null;
  index?: IndexEntry | null;
  checkpoint?: { since: number; minRev: number | null } | null;
}

export interface Store {
  close(): void;

  // checkpoint
  readCheckpoint(): Promise<CheckpointEntry | null>;
  writeCheckpoint(
    entry: Omit<CheckpointEntry, 'id' | 'updatedAt'> & { updatedAt?: number },
  ): Promise<CheckpointEntry>;

  // index
  getIndex(pathId: string): Promise<IndexEntry | null>;
  putIndex(entry: IndexEntry): Promise<void>;
  deleteIndex(pathId: string): Promise<void>;
  listIndex(): Promise<IndexEntry[]>;

  // base
  getBase(pathId: string): Promise<BaseEntry | null>;
  putBase(entry: BaseEntry): Promise<void>;
  deleteBase(pathId: string): Promise<void>;
  listBase(): Promise<BaseEntry[]>;

  // outbox
  putOutbox(entry: OutboxEntry): Promise<void>;
  putOutboxMany(entries: OutboxEntry[]): Promise<void>;
  getOutbox(operationId: string): Promise<OutboxEntry | null>;
  listOutbox(): Promise<OutboxEntry[]>;
  deleteOutbox(operationId: string): Promise<void>;
  descartarOutboxDePath(pathId: string, motivo: string): Promise<number>;
  setOutboxState(
    operationId: string,
    state: OutboxEntry['state'],
    lastError?: string | null,
  ): Promise<OutboxEntry>;
  confirmarLote(
    items: Array<{ entry: OutboxEntry; base: BaseEntry; index: IndexEntry }>,
    checkpoint: { since: number; minRev: number | null },
  ): Promise<void>;

  // transfers
  putTransfer(entry: TransferEntry): Promise<void>;
  getTransfer(transferId: string): Promise<TransferEntry | null>;
  listTransfers(): Promise<TransferEntry[]>;
  deleteTransfer(transferId: string): Promise<void>;

  // intents
  putIntent(entry: IntentEntry): Promise<void>;
  getIntent(intentId: string): Promise<IntentEntry | null>;
  listIntents(state?: IntentEntry['state']): Promise<IntentEntry[]>;
  deleteIntent(intentId: string): Promise<void>;
  markIntentVerified(intentId: string): Promise<IntentEntry>;
  completeApply(input: CompleteApplyInputContrato): Promise<void>;
}

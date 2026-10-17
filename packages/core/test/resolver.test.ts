/**
 * Tests de la resolución de conflictos (fase 5, corte mínimo):
 *  - el guard converge cuando el local YA coincide con el remoto (antes bloqueaba);
 *  - «remoto» pisa el local (apply forzado) y cierra la intención;
 *  - «local» deja el archivo intacto, avanza la base al remoto y cierra la intención.
 *
 * Los bloqueos se provocan con el flujo REAL (`applyRevision`) — nada de intents
 * fabricados a mano: el intentId es determinista y el resolver cuenta con eso.
 */

import { describe, expect, it } from 'vitest';

import { blobId, cryptoProvider, deriveSubkeys, encryptBlob, randomSalt, randomVaultKey } from '@ovaultd/crypto';
import { toHex } from '@ovaultd/protocol';

import { applyRevision, resolverBloqueado, type ApplyRevision, type ApplyDeps } from '../src/apply-runner.js';
import type { FileSystemPort } from '../src/file-port.js';
import type { BaseEntry, IndexEntry, IntentEntry, StateStore } from '../src/StateStore.js';
import type { SyncConnection } from '../src/SyncConnection.js';

const CLAVES = deriveSubkeys(randomVaultKey(), randomSalt());
const cod = new TextEncoder();
const dec = new TextDecoder();
const sha256Hex = (bytes: Uint8Array): string => toHex(cryptoProvider().sha256(bytes));

const PATH_ID = 'aa'.repeat(32);

// ------------------------------------------------------------------ fakes

function storeFake() {
  const intents = new Map<string, IntentEntry>();
  const base = new Map<string, BaseEntry>();
  const index = new Map<string, IndexEntry>();
  let checkpoint = { since: 0, minRev: null as number | null };

  const store = {
    async putIntent(entry: IntentEntry) {
      intents.set(entry.intentId, entry);
    },
    async getIntent(id: string) {
      return intents.get(id) ?? null;
    },
    async listIntents(state?: IntentEntry['state']) {
      return [...intents.values()].filter((i) => state === undefined || i.state === state);
    },
    async deleteIntent(id: string) {
      intents.delete(id);
    },
    async getBase(pathId: string) {
      return base.get(pathId) ?? null;
    },
    async getIndex(pathId: string) {
      return index.get(pathId) ?? null;
    },
    async completeApply(input: {
      intentId: string;
      base?: BaseEntry;
      index?: IndexEntry;
      checkpoint?: { since: number; minRev: number | null };
    }) {
      intents.delete(input.intentId);
      if (input.base) base.set(input.base.pathId, input.base);
      if (input.index) index.set(input.index.pathId, input.index);
      if (input.checkpoint) checkpoint = input.checkpoint;
    },
  };

  return { store, intents, base, index, checkpoint: () => checkpoint };
}

function portFake(archivos: Record<string, string>) {
  const files = new Map<string, Uint8Array>(
    Object.entries(archivos).map(([ruta, texto]) => [ruta, cod.encode(texto)]),
  );
  const temps = new Map<string, Uint8Array>();
  let tempSeq = 0;

  const port = {
    async stat(ruta: string) {
      const bytes = files.get(ruta);
      return bytes === undefined ? null : { isFile: true, isDirectory: false, size: bytes.length };
    },
    async read(ruta: string) {
      const bytes = files.get(ruta);
      if (bytes === undefined) throw new Error('no existe');
      return bytes;
    },
    async writeTemp(_dir: string, contenido: Uint8Array) {
      const tempName = `.ovaultd-tmp-${tempSeq++}`;
      temps.set(tempName, contenido);
      return { tempName };
    },
    async rename(tempName: string, ruta: string) {
      const bytes = temps.get(tempName);
      if (bytes === undefined) throw new Error('temporal ausente');
      temps.delete(tempName);
      files.set(ruta, bytes);
    },
    async unlink(ruta: string) {
      files.delete(ruta);
    },
    async mkdirp(_dir: string) {},
    async list(_dir: string) {
      return [];
    },
  };

  return { port, files };
}

function connFake(blobs: Map<string, Uint8Array>): SyncConnection {
  return {
    async getBlob(hex: string) {
      const bytes = blobs.get(hex);
      if (bytes === undefined) throw new Error('blob ausente');
      return bytes;
    },
  } as unknown as SyncConnection;
}

function depsDe(store: unknown, port: unknown): ApplyDeps {
  return { store: store as StateStore, port: port as FileSystemPort, keys: CLAVES };
}

const revisionDe = (blobIdHex: string): ApplyRevision => ({
  pathId: PATH_ID,
  path: 'nota.md',
  kind: 'file',
  deleted: false,
  rev: 6,
  blobId: blobIdHex,
  contentHash: null,
});

const baseVieja = (texto: string): BaseEntry => ({
  pathId: PATH_ID,
  rev: 3,
  blobId: null,
  contentHash: sha256Hex(cod.encode(texto)),
});

// ------------------------------------------------------------------ tests

describe('resolución de conflictos', () => {
  it('un local que ya coincide con el remoto converge (no se bloquea)', async () => {
    const remoto = cod.encode('contenido remoto');
    const almacenado = encryptBlob(CLAVES, remoto);
    const { store, base, checkpoint } = storeFake();
    const { port } = portFake({ 'nota.md': 'contenido remoto' });
    base.set(PATH_ID, baseVieja('versión vieja'));

    // Con el guard viejo esto bloqueaba aunque el local fuera idéntico al remoto.
    const resultado = await applyRevision(
      depsDe(store, port),
      revisionDe(toHex(blobId(almacenado))),
      almacenado,
    );

    expect(resultado.status).toBe('applied');
    expect(checkpoint().since).toBe(6);
  });

  it('«remoto» pisa el local con el contenido del server', async () => {
    const remoto = cod.encode('versión del server');
    const almacenado = encryptBlob(CLAVES, remoto);
    const blobIdHex = toHex(blobId(almacenado));

    const { store, base, checkpoint } = storeFake();
    const { port, files } = portFake({ 'nota.md': 'mi edición local' });
    base.set(PATH_ID, baseVieja('original'));
    const deps = depsDe(store, port);

    // Bloqueo REAL: local ≠ base ≠ remoto.
    const bloqueo = await applyRevision(deps, revisionDe(blobIdHex), almacenado);
    expect(bloqueo.status).toBe('blocked');
    const intent = await store.getIntent(bloqueo.intentId);
    expect(intent?.state).toBe('blocked');

    const resultado = await resolverBloqueado(
      deps,
      intent!,
      'remoto',
      connFake(new Map([[blobIdHex, almacenado]])),
    );

    expect(resultado.status).toBe('applied');
    expect(dec.decode(files.get('nota.md'))).toBe('versión del server');
    expect(await store.listIntents('blocked')).toHaveLength(0);
    expect(checkpoint().since).toBe(6);
  });

  it('«local» deja el archivo intacto y la base avanza al remoto', async () => {
    const remoto = cod.encode('versión del server');
    const almacenado = encryptBlob(CLAVES, remoto);
    const blobIdHex = toHex(blobId(almacenado));

    const { store, base, checkpoint } = storeFake();
    const { port, files } = portFake({ 'nota.md': 'mi edición local' });
    base.set(PATH_ID, baseVieja('original'));
    const deps = depsDe(store, port);

    const bloqueo = await applyRevision(deps, revisionDe(blobIdHex), almacenado);
    expect(bloqueo.status).toBe('blocked');
    const intent = await store.getIntent(bloqueo.intentId);

    const resultado = await resolverBloqueado(
      deps,
      intent!,
      'local',
      connFake(new Map([[blobIdHex, almacenado]])),
    );

    expect(resultado.status).toBe('applied');
    // El archivo local no se tocó…
    expect(dec.decode(files.get('nota.md'))).toBe('mi edición local');
    // …y la base quedó apuntando al remoto (el próximo push sube lo local como mod).
    expect(base.get(PATH_ID)?.contentHash).toBe(sha256Hex(remoto));
    expect(base.get(PATH_ID)?.rev).toBe(6);
    expect(await store.listIntents('blocked')).toHaveLength(0);
    expect(checkpoint().since).toBe(6);
  });
});

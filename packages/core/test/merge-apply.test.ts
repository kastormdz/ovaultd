/**
 * Merge 3-vías integrado al apply (fase 3, paso 4): cuando el guard bloquea por
 * "edición local posterior a la base" — el único caso que es de TRES versiones — el
 * apply intenta fusionar ANTES de declarar el conflicto.
 *
 * Lo que estos tests fijan:
 *  - la fusión se escribe al archivo local, y base/índice avanzan al REMOTO con su hash
 *    (el archivo queda "sucio" a propósito: el push lo sube como revisión nueva);
 *  - solapamiento real → conflicto explícito, archivo intacto;
 *  - sin conexión no hay fusión (el bridge de tests no la pasa): bloqueo como siempre;
 *  - la recuperación de un intent de fusión: commitea contra el hash remoto si el
 *    destino sigue siendo el local fusionado, y NO pisa si cambió.
 */

import { describe, expect, it } from 'vitest';

import { blobId, cryptoProvider, deriveSubkeys, encryptBlob, randomSalt, randomVaultKey } from '@ovaultd/crypto';
import { toHex } from '@ovaultd/protocol';

import { applyRevision, recoverApplies, type ApplyDeps, type ApplyRevision } from '../src/apply-runner.js';
import type { FileSystemPort } from '../src/file-port.js';
import type { BaseEntry, IndexEntry, IntentEntry, StateStore } from '../src/StateStore.js';
import type { SyncConnection } from '../src/SyncConnection.js';

const CLAVES = deriveSubkeys(randomVaultKey(), randomSalt());
const cod = new TextEncoder();
const dec = new TextDecoder();
const sha256Hex = (bytes: Uint8Array): string => toHex(cryptoProvider().sha256(bytes));

const PATH_ID = 'aa'.repeat(32);
const PATH = 'nota.md';

// ------------------------------------------------------------------ fakes
// (mismo estilo que resolver.test.ts: store/port/conn en memoria)

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

  // El port real (adapter del FS) ve los temporales como archivos: `stat`/`read` tienen
  // que mirar los dos mapas o el recovery "no vería" el temp que existe.
  const ver = (ruta: string): Uint8Array | undefined => files.get(ruta) ?? temps.get(ruta);

  const port = {
    async stat(ruta: string) {
      const bytes = ver(ruta);
      return bytes === undefined ? null : { isFile: true, isDirectory: false, size: bytes.length };
    },
    async read(ruta: string) {
      const bytes = ver(ruta);
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
      temps.delete(ruta);
    },
    async mkdirp(_dir: string) {},
    async list(_dir: string) {
      return [];
    },
  };

  return { port, files, temps };
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

// ------------------------------------------------------------------ el caso

const BASE_TEXTO = 'El gato duerme en el sofá.\n';
const OURS_TEXTO = 'El gato duerme en el sofá rojo.\n';
const THEIRS_TEXTO = 'El gato grande duerme en el sofá.\n';
const FUSION_TEXTO = 'El gato grande duerme en el sofá rojo.\n';

/** Arma el escenario de tres versiones: base en el store, local en el port, remoto en el CAS. */
function armarCaso(opciones: { local: string; remoto: string }) {
  const storedBase = encryptBlob(CLAVES, cod.encode(BASE_TEXTO));
  const blobIdBase = toHex(blobId(storedBase));
  const storedRemoto = encryptBlob(CLAVES, cod.encode(opciones.remoto));

  const { store, intents, base, index, checkpoint } = storeFake();
  base.set(PATH_ID, {
    pathId: PATH_ID,
    rev: 3,
    blobId: blobIdBase,
    contentHash: sha256Hex(cod.encode(BASE_TEXTO)),
  });

  const { port, files, temps } = portFake({ [PATH]: opciones.local });
  const conn = connFake(new Map([[blobIdBase, storedBase]]));

  const deps: ApplyDeps = {
    store: store as unknown as StateStore,
    port: port as unknown as FileSystemPort,
    keys: CLAVES,
    conn,
  };
  const revision: ApplyRevision = {
    pathId: PATH_ID,
    path: PATH,
    kind: 'file',
    deleted: false,
    rev: 6,
    blobId: toHex(blobId(storedRemoto)),
    contentHash: null,
  };

  return { deps, revision, storedRemoto, files, temps, intents, base, index, checkpoint, store };
}

// ------------------------------------------------------------------ tests

describe('merge 3-vías integrado al apply', () => {
  it('ediciones disjuntas: fusiona, y el archivo queda sucio contra el remoto para el push', async () => {
    const caso = armarCaso({ local: OURS_TEXTO, remoto: THEIRS_TEXTO });

    const resultado = await applyRevision(caso.deps, caso.revision, caso.storedRemoto);
    expect(resultado.status).toBe('merged');

    // El archivo local: la FUSIÓN de las dos ediciones.
    expect(dec.decode(caso.files.get(PATH))).toBe(FUSION_TEXTO);

    // Base e índice avanzan al REMOTO con SU hash: el archivo difiere a propósito.
    expect(caso.index.get(PATH_ID)?.contentHash).toBe(sha256Hex(cod.encode(THEIRS_TEXTO)));
    expect(caso.index.get(PATH_ID)?.rev).toBe(6);
    expect(caso.base.get(PATH_ID)?.contentHash).toBe(sha256Hex(cod.encode(THEIRS_TEXTO)));

    // El checkpoint avanza a la remota: el pull puede seguir.
    expect(caso.checkpoint()).toEqual({ since: 6, minRev: null });

    // La consecuencia que importa: el archivo NO coincide con el índice (el scan lo ve
    // sucio → el push sube la fusión como revisión NUEVA).
    expect(sha256Hex(caso.files.get(PATH)!)).not.toBe(caso.index.get(PATH_ID)?.contentHash);
  });

  it('la misma frase editada distinto: conflicto explícito y el local intacto', async () => {
    const caso = armarCaso({ local: 'El gato ronca en el sofá.\n', remoto: 'El gato corre en el sofá.\n' });

    const resultado = await applyRevision(caso.deps, caso.revision, caso.storedRemoto);
    expect(resultado.status).toBe('blocked');
    expect(resultado.reason).toContain('edición local posterior a la base');

    // El archivo local quedó INTACTO y la base sigue donde estaba.
    expect(dec.decode(caso.files.get(PATH))).toBe('El gato ronca en el sofá.\n');
    expect(caso.base.get(PATH_ID)?.rev).toBe(3);
  });

  it('modo espejo (forzar): pisa el local y resguarda lo descartado en .ovaultd-descartes', async () => {
    const caso = armarCaso({ local: OURS_TEXTO, remoto: THEIRS_TEXTO });

    // El pull del modo mirror-remote aplica con `forzar`: la versión del server manda.
    const resultado = await applyRevision(caso.deps, caso.revision, caso.storedRemoto, {
      forzar: true,
    });
    expect(['applied', 'merged']).toContain(resultado.status);

    // El archivo quedó con la versión del SERVER…
    expect(dec.decode(caso.files.get(PATH))).toBe(THEIRS_TEXTO);
    // …y lo descartado está resguardado en `.ovaultd-descartes/<sello>/nota.md`
    // (carpeta vetada por el filtro: jamás vuelve al server).
    const descartes = [...caso.files.entries()].filter(([ruta]) =>
      ruta.startsWith('.ovaultd-descartes/'),
    );
    expect(descartes).toHaveLength(1);
    expect(dec.decode(descartes[0]![1])).toBe(OURS_TEXTO);
  });

  it('sin conexión no fusiona: el bloqueo explícito de siempre', async () => {
    const caso = armarCaso({ local: OURS_TEXTO, remoto: THEIRS_TEXTO });
    const { conn: _sin, ...depsSinConn } = caso.deps;

    const resultado = await applyRevision(depsSinConn as ApplyDeps, caso.revision, caso.storedRemoto);
    expect(resultado.status).toBe('blocked');
    expect(dec.decode(caso.files.get(PATH))).toBe(OURS_TEXTO);
  });

  it('los binarios y canvas no se fusionan aunque no haya solapamiento', async () => {
    const caso = armarCaso({ local: OURS_TEXTO, remoto: THEIRS_TEXTO });
    caso.files.set('tablero.canvas', cod.encode('{"nodes":[]}'));

    const resultado = await applyRevision(
      caso.deps,
      { ...caso.revision, path: 'tablero.canvas' },
      caso.storedRemoto,
    );
    // No hay base para ese path: el bloqueo es por "dos creaciones", tampoco fusionable.
    expect(resultado.status).toBe('blocked');
  });

  it('la recuperación de un intent de fusión commitea contra el hash REMOTO', async () => {
    // Estado que deja un crash tras el writeTemp y ANTES del rename: temp verificado con
    // la fusión, destino todavía con el local, intent con `mergeBase`.
    const caso = armarCaso({ local: OURS_TEXTO, remoto: THEIRS_TEXTO });
    const { tempName } = await caso.deps.port.writeTemp('.', cod.encode(FUSION_TEXTO));
    const intent: IntentEntry = {
      intentId: 'merge-crash-1',
      pathId: PATH_ID,
      rev: 6,
      blobId: caso.revision.blobId!,
      state: 'verified',
      path: PATH,
      contentHash: sha256Hex(cod.encode(FUSION_TEXTO)),
      tempName,
      mergeBase: {
        rev: 6,
        blobId: caso.revision.blobId,
        contentHash: sha256Hex(cod.encode(THEIRS_TEXTO)),
        oursHash: sha256Hex(cod.encode(OURS_TEXTO)),
      },
      createdAt: Date.now(),
    };
    await caso.store.putIntent(intent);

    const reporte = await recoverApplies(caso.deps);
    expect(reporte.completed).toBe(1);
    expect(dec.decode(caso.files.get(PATH))).toBe(FUSION_TEXTO);
    // El commit usó `mergeBase`: el índice quedó con el hash remoto (archivo sucio).
    expect(caso.index.get(PATH_ID)?.contentHash).toBe(sha256Hex(cod.encode(THEIRS_TEXTO)));
  });

  it('si el destino cambió después del crash, la recuperación NO pisa', async () => {
    const caso = armarCaso({ local: OURS_TEXTO, remoto: THEIRS_TEXTO });
    const { tempName } = await caso.deps.port.writeTemp('.', cod.encode(FUSION_TEXTO));
    await caso.store.putIntent({
      intentId: 'merge-crash-2',
      pathId: PATH_ID,
      rev: 6,
      blobId: caso.revision.blobId!,
      state: 'verified',
      path: PATH,
      contentHash: sha256Hex(cod.encode(FUSION_TEXTO)),
      tempName,
      mergeBase: {
        rev: 6,
        blobId: caso.revision.blobId,
        contentHash: sha256Hex(cod.encode(THEIRS_TEXTO)),
        oursHash: sha256Hex(cod.encode(OURS_TEXTO)),
      },
      createdAt: Date.now(),
    });
    // Alguien más tocó el archivo entre el crash y la recuperación:
    caso.files.set(PATH, cod.encode('editado después del crash'));

    const reporte = await recoverApplies(caso.deps);
    expect(reporte.blocked).toBe(1);
    expect(dec.decode(caso.files.get(PATH))).toBe('editado después del crash');
  });
});

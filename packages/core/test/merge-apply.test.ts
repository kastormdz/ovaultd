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

import { applyRevision, pathDeCopiaDeConflicto, recoverApplies, type ApplyDeps, type ApplyRevision } from '../src/apply-runner.js';
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
  const descartados: string[] = [];
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
    async descartarOutboxDePath(pathId: string) {
      descartados.push(pathId);
      return 0;
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

  return { store, intents, base, index, descartados, checkpoint: () => checkpoint };
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
function armarCaso(opciones: {
  local: string;
  remoto: string;
  base?: string;
  ruta?: string;
  pathId?: string;
}) {
  const ruta = opciones.ruta ?? PATH;
  const pathId = opciones.pathId ?? PATH_ID;
  const baseTexto = opciones.base ?? BASE_TEXTO;
  const storedBase = encryptBlob(CLAVES, cod.encode(baseTexto));
  const blobIdBase = toHex(blobId(storedBase));
  const storedRemoto = encryptBlob(CLAVES, cod.encode(opciones.remoto));

  const { store, intents, base, index, descartados, checkpoint } = storeFake();
  base.set(pathId, {
    pathId,
    rev: 3,
    blobId: blobIdBase,
    contentHash: sha256Hex(cod.encode(baseTexto)),
  });

  const { port, files, temps } = portFake({ [ruta]: opciones.local });
  const conn = connFake(new Map([[blobIdBase, storedBase]]));

  const deps: ApplyDeps = {
    store: store as unknown as StateStore,
    port: port as unknown as FileSystemPort,
    keys: CLAVES,
    conn,
    // Los casos de conflicto de este archivo prueban la política MANUAL (bloquear); la
    // política por defecto (conservar copia) tiene sus propios tests al final.
    politicaDeConflictos: 'bloquear',
  };
  const revision: ApplyRevision = {
    pathId,
    path: ruta,
    kind: 'file',
    deleted: false,
    rev: 6,
    blobId: toHex(blobId(storedRemoto)),
    contentHash: null,
  };

  return { deps, revision, storedRemoto, files, temps, intents, base, index, descartados, checkpoint, store };
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

    // El checkpoint NO se mueve acá: lo cierra `pullOnce` cuando la revisión entera quedó
    // aplicada (una rev trae N ops y avanzar por registro dejaba el resto inalcanzable).
    expect(caso.checkpoint()).toEqual({ since: 0, minRev: null });

    // La consecuencia que importa: el archivo NO coincide con el índice (el scan lo ve
    // sucio → el push sube la fusión como revisión NUEVA).
    expect(sha256Hex(caso.files.get(PATH)!)).not.toBe(caso.index.get(PATH_ID)?.contentHash);
  });

  it('local ausente: la revisión remota se APLICA y el ciclo no se bloquea', async () => {
    // El archivo estaba en la base pero ya no está en disco (un theme borrado a mano, una
    // config que Obsidian limpió). El guard viejo lo trataba como conflicto y congelaba el
    // ciclo ENTERO para siempre; ahora se escribe la versión del server.
    const caso = armarCaso({ local: OURS_TEXTO, remoto: THEIRS_TEXTO });
    caso.files.delete(PATH); // el disco ya no lo tiene

    const resultado = await applyRevision(caso.deps, caso.revision, caso.storedRemoto);

    expect(resultado.status).not.toBe('blocked');
    expect(dec.decode(caso.files.get(PATH))).toBe(THEIRS_TEXTO);
    expect(caso.checkpoint()).toEqual({ since: 0, minRev: null });
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

  it('appearance.json: el tema del remoto gana y el modo local se conserva', async () => {
    const PATH_AP = '.obsidian/appearance.json';
    const PATH_ID_AP = 'bb'.repeat(32);
    const baseTexto = JSON.stringify({ cssTheme: 'Anu', theme: 'obsidian', baseFontSize: 16 });
    // Obsidian reescribe appearance.json con SU orden de claves: el texto cambia entero
    // aunque semánticamente solo haya cambiado el modo. Es el caso real.
    const localTexto = JSON.stringify({ theme: 'moonstone', baseFontSize: 18, cssTheme: 'Anu' });
    const remotoTexto = JSON.stringify({ cssTheme: 'Cosmic', theme: 'obsidian', baseFontSize: 16 });

    const storedBase = encryptBlob(CLAVES, cod.encode(baseTexto));
    const blobIdBase = toHex(blobId(storedBase));
    const storedRemoto = encryptBlob(CLAVES, cod.encode(remotoTexto));

    const { store, base } = storeFake();
    base.set(PATH_ID_AP, {
      pathId: PATH_ID_AP,
      rev: 3,
      blobId: blobIdBase,
      contentHash: sha256Hex(cod.encode(baseTexto)),
    });
    const { port, files } = portFake({ [PATH_AP]: localTexto });
    const conn = connFake(new Map([[blobIdBase, storedBase]]));

    const deps: ApplyDeps = {
      store: store as unknown as StateStore,
      port: port as unknown as FileSystemPort,
      keys: CLAVES,
      conn,
      politicaDeConflictos: 'bloquear',
    };
    const revision: ApplyRevision = {
      pathId: PATH_ID_AP,
      path: PATH_AP,
      kind: 'file',
      deleted: false,
      rev: 6,
      blobId: toHex(blobId(storedRemoto)),
      contentHash: null,
    };

    const resultado = await applyRevision(deps, revision, storedRemoto);

    expect(resultado.status).toBe('merged');
    const final = JSON.parse(dec.decode(files.get(PATH_AP)!)) as Record<string, unknown>;
    expect(final.cssTheme).toBe('Cosmic'); // el tema viaja
    expect(final.theme).toBe('moonstone'); // el modo local se conserva
    expect(final.baseFontSize).toBe(18); // y el tamaño también
  });

describe('conflictos: política por defecto (conservar copia, estilo Obsidian Sync)', () => {
  it('el path del conflicto: sello de 12 dígitos, extensión y dispositivo saneado', () => {
    const cuando = new Date('2026-10-19T20:45:31Z');
    expect(pathDeCopiaDeConflicto('Notas/uno.md', 'escritorio', cuando)).toBe(
      'Notas/uno (conflicto escritorio 202610192045).md',
    );
    expect(pathDeCopiaDeConflicto('README', 'Teléfono S23', cuando)).toBe(
      'README (conflicto Tel-fono-S23 202610192045)',
    );
    expect(pathDeCopiaDeConflicto('a/b/c.canvas', 'x y', cuando, 2)).toBe(
      'a/b/c (conflicto x-y 202610192045-2).canvas',
    );
  });

  it('conflicto de contenido: el remoto queda en el path y el local en la copia', async () => {
    // La MISMA frase editada distinto: el merge no puede decidir y cae al fallback.
    const caso = armarCaso({
      local: 'El gato ronca en el sofá.\n',
      remoto: 'El gato corre en el sofá.\n',
    });
    const deps: ApplyDeps = {
      ...caso.deps,
      politicaDeConflictos: 'conservar',
      deviceName: 'escritorio',
    };

    const resultado = await applyRevision(deps, caso.revision, caso.storedRemoto);

    expect(resultado.status).toBe('conflicted-copy');
    expect(resultado.copia).toBeTruthy();
    // El path original: la versión del SERVER (gana el remoto).
    expect(dec.decode(caso.files.get(PATH))).toBe('El gato corre en el sofá.\n');
    // La copia: la edición LOCAL, intacta.
    expect(dec.decode(caso.files.get(resultado.copia!))).toBe('El gato ronca en el sofá.\n');
    // Sin checkpoint en el apply: el ciclo no se bloquea y `pullOnce` lo cierra al final.
    expect(caso.checkpoint()).toEqual({ since: 0, minRev: null });
    // El intent quedó cerrado (no hay nada bloqueado pendiente).
    expect([...caso.intents.values()].filter((i) => i.state === 'blocked')).toHaveLength(0);
  });

  it('sin conexión también conserva: la copia no necesita red', async () => {
    const caso = armarCaso({
      local: 'El gato ronca en el sofá.\n',
      remoto: 'El gato corre en el sofá.\n',
    });
    const { conn: _sin, ...sinConn } = caso.deps;
    const deps: ApplyDeps = {
      ...(sinConn as ApplyDeps),
      politicaDeConflictos: 'conservar',
      deviceName: 'telefono',
    };

    const resultado = await applyRevision(deps, caso.revision, caso.storedRemoto);
    expect(resultado.status).toBe('conflicted-copy');
    expect(dec.decode(caso.files.get(PATH))).toBe('El gato corre en el sofá.\n');
    expect(dec.decode(caso.files.get(resultado.copia!))).toBe('El gato ronca en el sofá.\n');
  });

  it('un BORRADO remoto contra un local editado sigue bloqueando (no se automatiza)', async () => {
    const caso = armarCaso({ local: OURS_TEXTO, remoto: THEIRS_TEXTO });
    const deps: ApplyDeps = { ...caso.deps, politicaDeConflictos: 'conservar', deviceName: 'escritorio' };

    const resultado = await applyRevision(
      deps,
      { ...caso.revision, deleted: true, blobId: null },
      null,
    );

    // Conservar una copia y ADEMÁS borrar el original no es una decisión automática.
    expect(resultado.status).toBe('blocked');
    expect(dec.decode(caso.files.get(PATH))).toBe(OURS_TEXTO);
  });

  it('la política «bloquear» mantiene el comportamiento manual', async () => {
    const caso = armarCaso({
      local: 'El gato ronca en el sofá.\n',
      remoto: 'El gato corre en el sofá.\n',
    });
    const deps: ApplyDeps = { ...caso.deps, politicaDeConflictos: 'bloquear', deviceName: 'escritorio' };

    const resultado = await applyRevision(deps, caso.revision, caso.storedRemoto);

    expect(resultado.status).toBe('blocked');
    expect(resultado.reason).toContain('edición local posterior a la base');
    expect(dec.decode(caso.files.get(PATH))).toBe('El gato ronca en el sofá.\n');
  });
});

describe('apply desaloja la intención pendiente del path', () => {
  it('al aplicar la versión remota descarta la outbox de ese path', async () => {
    // Local idéntico a la base: el guard da `safe` y el apply escribe la versión remota.
    const caso = armarCaso({ local: BASE_TEXTO, remoto: 'texto nuevo\n' });

    await applyRevision(caso.deps, caso.revision, caso.storedRemoto);

    expect(caso.descartados).toContain(PATH_ID);
  });
});

describe('appearance.json: el modo es del dispositivo', () => {
  const BASE_APARIENCIA = `${JSON.stringify(
    { theme: 'moonstone', cssTheme: '', enabledCssSnippets: [], baseFontSize: 16 },
    null,
    2,
  )}\n`;
  const REMOTA_APARIENCIA = `${JSON.stringify(
    { theme: 'obsidian', cssTheme: 'Minimal', enabledCssSnippets: ['grande'], baseFontSize: 16 },
    null,
    2,
  )}\n`;
  const RUTA = '.obsidian/appearance.json';

  it('aplica los campos portables y conserva el tema del dispositivo', async () => {
    const caso = armarCaso({
      local: BASE_APARIENCIA,
      base: BASE_APARIENCIA,
      remoto: REMOTA_APARIENCIA,
      ruta: RUTA,
    });

    const resultado = await applyRevision(caso.deps, caso.revision, caso.storedRemoto);
    expect(resultado.status).toBe('applied');

    const escrito = JSON.parse(dec.decode(caso.files.get(RUTA))) as Record<string, unknown>;
    expect(escrito.theme).toBe('moonstone'); // del local, no se pisa
    expect(escrito.cssTheme).toBe('Minimal'); // portable, viene del remoto
    expect(escrito.enabledCssSnippets).toEqual(['grande']);

    // Base e índice quedan con el hash de lo PUBLICADO (si no, el scan lo ve sucio y
    // vuelve a subir el tema en cada ciclo).
    const publicado = dec.decode(caso.files.get(RUTA));
    expect(caso.base.get(caso.revision.pathId)?.contentHash).toBe(
      sha256Hex(cod.encode(publicado)),
    );
    expect(caso.index.get(caso.revision.pathId)?.contentHash).toBe(
      sha256Hex(cod.encode(publicado)),
    );
  });
});

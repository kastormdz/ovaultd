/**
 * Push — la mitad de subida del ciclo offline (fase 2, paso 4).
 *
 * Flujo (save-first, paso 2 del plan):
 *   1. Escaneo + hash-diff contra la base.
 *   2. Cada cambio se **encola** (snapshot Blob verificado) ANTES de publicar nada.
 *   3. Drenado, una operación por batch: cifrar (ADR-0014) → subir blob (HTTP) →
 *      firmar `msg_rec` → commit por WS. El `expected_rev` es el head VIVO: arranca en
 *      el checkpoint y avanza con cada ack de NUESTRAS operaciones (si otro device
 *      commitea en el medio, el CAS da `conflict` — que es exactamente lo que queremos).
 *   4. Ack: base + índice avanzan y la entrada pasa a confirmed (se borra).
 *      Conflict: se surfacea y la operación queda `pending` (resolución fina = fase 5).
 *
 * El `operation_id` es el de la entrada de outbox: determinista y persistido, así el
 * reintento del MISMO intento (crash entre commit y ack incluido) reusa la operación y
 * el server la responde idempotente.
 *
 * Un batch por operación en este corte: el protocolo permite hasta 5 ops por revisión,
 * pero la corrección primero — agrupar es una optimización con CAS más delicado.
 */

import {
  blobId as blobIdDe,
  bytesToUuid,
  cryptoProvider,
  encryptBlob,
  identityFromSubkeys,
  pathEnc,
  signRecord,
  tMac,
} from '@ovaultd/crypto';
import {
  b64Encode,
  fromHex,
  signOpCode,
  toHex,
  type BatchOpWire,
  type BatchWire,
} from '@ovaultd/protocol';

import type { ApplyDeps } from './apply-runner.js';
import { prepareOutboxEntry } from './outbox.js';
import { diffLocal, escanearVault } from './scan.js';
import { SyncConflictError, type SyncConnection } from './SyncConnection.js';
import type { BaseEntry, IndexEntry, OutboxEntry } from './StateStore.js';

export interface PushDeps extends ApplyDeps {
  conn: SyncConnection;
  /** `vault_id` de 16 B (el del material, no el numérico del server). */
  vaultUuid: Uint8Array;
  epoch: number;
}

export interface PushReport {
  diff: {
    nuevos: number;
    modificados: number;
    borrados: number;
    sinCambios: number;
    yaEncolados: number;
  };
  encoladas: number;
  empujadas: number;
  headRev: number;
  /**
   * Deletes que la guarda del cliente vacío NO encoló: sin ningún archivo local, los
   * "borrados" del diff son un artefacto (vault recién creado, disco sin montar, scan a
   * medias), no una intención del usuario.
   */
  deletesBloqueados: number;
  conflicto?: { operationId: string; path?: string; mensaje: string };
}

const CERO32 = new Uint8Array(32);
const CERO64 = new Uint8Array(64);

/**
 * Ops por batch. El server acepta hasta MAX_BATCH_OPS (1000). Lotes grandes amortizan el
 * costo FIJO del commit (un round-trip WS + una transacción + un `syncfs`: ~450-600 ms
 * medidos por lote, casi independiente del tamaño); el aislamiento de un conflicto sigue
 * siendo por op (se reintenta de a una si el lote falla).
 */
const TAMANO_LOTE = 1000;

/** Parte una lista en lotes de a lo sumo `tamano`. */
function enLotes<T>(items: T[], tamano: number): T[][] {
  const lotes: T[][] = [];
  for (let i = 0; i < items.length; i += tamano) lotes.push(items.slice(i, i + tamano));
  return lotes;
}

/** `operation_id` textual (UUID) → los 16 bytes que firma `msg_rec`. */
const operacionBytes = (operationId: string): Uint8Array => {
  const hex = operationId.replace(/-/g, '');
  if (!/^[0-9a-fA-F]{32}$/.test(hex)) {
    throw new Error(`operation_id no es un UUID de 16 B: ${operationId}`);
  }
  return fromHex(hex);
};

/**
 * Encola el diff en la outbox (save-first: el snapshot queda persistido y verificado
 * antes de cualquier publicación).
 */
async function encolarDiff(deps: PushDeps, head: number): Promise<{ diff: PushReport['diff']; encoladas: number; deletesBloqueados: number }> {
  const { store, port, keys } = deps;
  // Cache del escaneo incremental: el índice guarda mtime+size+hash de lo último visto
  // (el push lo escribe al confirmar); si nada cambió, el scan no lee el contenido.
  const cache = new Map(
    (await store.listIndex()).map((entrada) => [
      entrada.path ?? '',
      { mtime: entrada.mtime ?? null, size: entrada.size ?? null, contentHash: entrada.contentHash ?? null },
    ]),
  );
  const contenidos = new Map<string, Uint8Array>();
  const tScan = performance.now();
  const entradas = await escanearVault(port, keys, '', cache, contenidos);
  marcarFase('scan', performance.now() - tScan);
  const tDiff = performance.now();
  const diffBase = await diffLocal(store, entradas);
  marcarFase('diff', performance.now() - tDiff);

  // Un cliente VACÍO no borra el remoto (aceptación de fase 3): sin un solo archivo en
  // disco, los deletes del diff no son una intención — se bloquean y se informan.
  const deletesBloqueados = entradas.length === 0 ? diffBase.borrados.length : 0;
  const diff = {
    ...diffBase,
    borrados: deletesBloqueados > 0 ? [] : diffBase.borrados,
  };
  let encoladas = 0;
  const pendientesDeEncolar: OutboxEntry[] = [];

  const encolar = async (entry: OutboxEntry): Promise<void> => {
    // Idempotencia por payload: el MISMO cambio no duplica la operación. Las altas se
    // acumulan y se escriben en UNA transacción al final (una por entrada costaba ~6 ms).
    const tGet = performance.now();
    const yaEsta = await store.getOutbox(entry.operationId);
    marcarFase('getOutbox', performance.now() - tGet);
    if (yaEsta) return;
    pendientesDeEncolar.push(entry);
  };

  for (const archivo of diff.nuevos) {
    // Los bytes YA vienen del escaneo (readMany): no se vuelve a pedir el archivo al
    // puente. El Map se vacía a medida que se consume para no retener el vault entero.
    const tLeer = performance.now();
    const bytes = contenidos.get(archivo.path) ?? (await port.read(archivo.path));
    contenidos.delete(archivo.path);
    marcarFase('leer', performance.now() - tLeer);
    await encolar(
      prepareOutboxEntry({
        kind: 'add',
        path: archivo.path,
        pathId: archivo.pathId,
        expectedRev: head,
        contentB64: b64Encode(bytes),
        contentHash: archivo.contentHash,
        mtimeMs: archivo.mtimeMs,
      }),
    );
  }
  for (const archivo of diff.modificados) {
    const tLeer = performance.now();
    const bytes = contenidos.get(archivo.path) ?? (await port.read(archivo.path));
    contenidos.delete(archivo.path);
    marcarFase('leer', performance.now() - tLeer);
    await encolar(
      prepareOutboxEntry({
        kind: 'mod',
        path: archivo.path,
        pathId: archivo.pathId,
        expectedRev: head,
        contentB64: b64Encode(bytes),
        contentHash: archivo.contentHash,
        mtimeMs: archivo.mtimeMs,
      }),
    );
  }
  for (const borrado of diff.borrados) {
    await encolar(
      prepareOutboxEntry({
        kind: 'del',
        path: borrado.path ?? '(desconocido)',
        pathId: borrado.pathId,
        expectedRev: head,
      }),
    );
  }

  // Las altas del diff, en UNA transacción de IndexedDB.
  const tEncolar = performance.now();
  await store.putOutboxMany(pendientesDeEncolar);
  marcarFase('encolar', performance.now() - tEncolar);
  encoladas += pendientesDeEncolar.length;

  return {
    diff: {
      nuevos: diff.nuevos.length,
      modificados: diff.modificados.length,
      borrados: diff.borrados.length,
      sinCambios: diff.sinCambios,
      yaEncolados: diff.yaEncolados,
    },
    encoladas,
    deletesBloqueados,
  };
}

/** Construye la operación del wire (cifra + firma) para una entrada de outbox. */
async function construirOp(
  deps: PushDeps,
  entry: OutboxEntry,
  expectedRev: number,
  /**
   * `operation_id` CON EL QUE SE FIRMA: el del BATCH. El `msg_rec` lo incluye, y el
   * server verifica cada op del lote contra el `operation_id` del batch — firmar con el
   * de la entrada individual delata la firma (unauthorized) en cuanto el lote trae más
   * de una operación.
   */
  operationId: Uint8Array,
): Promise<{ op: BatchOpWire; blobIdHex: string | null }> {
  const { conn, keys, vaultUuid, epoch } = deps;
  const identidad = identityFromSubkeys(keys);
  const pathIdBytes = fromHex(entry.pathId);
  const esDel = entry.kind === 'del';

  let blobIdHex: string | null = null;
  let blobIdBytes: Uint8Array = CERO32;
  let tamano = 0;

  if (!esDel) {
    if (!entry.snapshot || !entry.snapshotHash) {
      throw new Error(`outbox ${entry.operationId}: sin snapshot recuperable`);
    }
    const plano = new Uint8Array(await entry.snapshot.arrayBuffer());
    const real = toHex(cryptoProvider().sha256(plano));
    if (real !== entry.snapshotHash) {
      throw new Error(
        `snapshot corrupto en ${entry.operationId} (declarado ${entry.snapshotHash}, real ${real})`,
      );
    }
    const tPrep = performance.now();
    const almacenado = encryptBlob(keys, plano);
    const idBytes = blobIdDe(almacenado);
    blobIdHex = toHex(idBytes);
    blobIdBytes = idBytes;
    tamano = plano.length;
    marcarFase('preparar', performance.now() - tPrep);
    const tPut = performance.now();
    await conn.putBlob(blobIdHex, almacenado);
    marcarFase('put', performance.now() - tPut);
  }

  const tmacBytes = esDel ? CERO64 : tMac(identidad, vaultUuid, pathIdBytes, blobIdBytes, epoch);
  const sig = signRecord(
    identidad,
    pathIdBytes,
    blobIdBytes,
    tmacBytes,
    signOpCode(entry.kind),
    tamano,
    // Un `del` no lleva mtime en el wire: el server reconstruye el mensaje con 0
    // (`op.mtime.unwrap_or(0)`) — firmar con el queuedAt delata la firma.
    esDel ? 0 : entry.queuedAt,
    expectedRev,
    operationId,
    epoch,
  );

  const op: BatchOpWire = {
    op: entry.kind,
    path_id: b64Encode(pathIdBytes),
    path_enc: esDel ? null : b64Encode(pathEnc(keys, entry.path ?? '')),
    folder: false,
    blob_id: esDel ? null : b64Encode(blobIdBytes),
    t_mac: esDel ? null : b64Encode(tmacBytes),
    sig: b64Encode(sig),
    size: esDel ? null : tamano,
    mtime: esDel ? null : entry.queuedAt,
    ctime: null,
    prev_path_id: null,
  };

  return { op, blobIdHex };
}

/**
 * Arma las filas de base e índice que el lote confirma de una sola transacción. El mtime
 * es el REAL del archivo (no el de encolado): es lo que compara el escaneo incremental
 * para decidir si puede reusar el hash sin leer el contenido.
 */
function datosDeConfirmacion(
  entry: OutboxEntry,
  rev: number,
  blobIdHex: string | null,
): { entry: OutboxEntry; base: BaseEntry; index: IndexEntry } {
  const esDel = entry.kind === 'del';
  return {
    entry,
    base: {
      pathId: entry.pathId,
      rev,
      blobId: esDel ? null : blobIdHex,
      contentHash: esDel ? null : (entry.snapshotHash ?? null),
    },
    index: {
      pathId: entry.pathId,
      // El path ya viene en la entrada (lo puso el diff).
      path: entry.path ?? '',
      kind: 'file',
      deleted: esDel,
      rev,
      blobId: esDel ? null : blobIdHex,
      size: entry.snapshot ? entry.snapshot.size : null,
      mtime: entry.mtimeMs ?? entry.queuedAt,
      contentHash: esDel ? null : (entry.snapshotHash ?? null),
    },
  };
}

/** Una pasada de subida: escanear → encolar → drenar. No reconecta (eso es del llamador). */
/**
 * Tandas con concurrencia ACOTADA. Con 50 PUT de 133 KB en vuelo, el puente (un solo
 * proceso node que reenvía cada request) y el navegador se ahogan: medido 330 ms por PUT
 * contra 6 ms del server. Ocho en vuelo mantienen el pipelining sin saturar al que media.
 */
const CONCURRENCIA_PUT = 24;

async function enTandas<T, R>(
  items: T[],
  limite: number,
  fn: (item: T) => Promise<R>,
): Promise<R[]> {
  const resultado: R[] = new Array(items.length);
  let siguiente = 0;
  const trabajadores = Array.from({ length: Math.min(limite, items.length) }, async () => {
    for (;;) {
      const i = siguiente;
      siguiente += 1;
      if (i >= items.length) return;
      resultado[i] = await fn(items[i]!);
    }
  });
  await Promise.all(trabajadores);
  return resultado;
}

/**
 * Instrumentación de fases del push (solo diagnóstico): acumula milisegundos por etapa y
 * al final del ciclo los manda al puente, que los imprime en el stderr del CLI. Sirve
 * para decidir con números dónde está el costo, no por módulo.
 */
const FASES = {
  scan: 0,
  diff: 0,
  leer: 0,
  encolar: 0,
  preparar: 0,
  commit: 0,
  put: 0,
  listOutbox: 0,
  getOutbox: 0,
  confirmar: 0,
  total: 0,
};

function marcarFase(nombre: keyof typeof FASES, ms: number): void {
  FASES[nombre] += ms;
}

export async function pushOnce(deps: PushDeps): Promise<PushReport> {
  for (const clave of Object.keys(FASES) as Array<keyof typeof FASES>) FASES[clave] = 0;
  const t0Total = performance.now();
  const { store } = deps;
  let head = (await store.readCheckpoint().catch(() => null))?.since ?? 0;

  const { diff, encoladas, deletesBloqueados } = await encolarDiff(deps, head);
  let empujadas = 0;

  const tLista = performance.now();
  const pendientes = (await store.listOutbox())
    .filter((entry) => entry.state !== 'confirmed')
    .sort((a, b) => a.queuedAt - b.queuedAt);
  marcarFase('listOutbox', performance.now() - tLista);
  /**
   * Sube un grupo de operaciones en UN batch (el server valida `expected_rev` una sola vez
   * para todo el lote y asigna las revisiones consecutivas: sigue habiendo UNA por op, así
   * que el history, el delta y los expected_rev no cambian). Las operaciones se construyen
   * EN PARALELO: cada una cifra, sube su blob por HTTP PUT y firma — los PUTs concurrentes
   * son el grueso de la ganancia en la subida inicial.
   */
  const subir = async (lote: OutboxEntry[]): Promise<void> => {
    // El `msg_rec` firma el operation_id DEL BATCH: todas las ops se firman con el mismo
    // (el de la primera entrada — estable, así el retry de un lote es idempotente).
    const operationIdDelLote = lote[0]!.operationId;
    const operacionDelLote = operacionBytes(operationIdDelLote);
    const construidas = await enTandas(lote, CONCURRENCIA_PUT, (entry) =>
      construirOp(deps, entry, head, operacionDelLote),
    );

    const batch: BatchWire = {
      t: 'batch',
      batch_id: bytesToUuid(cryptoProvider().randomBytes(16)),
      operation_id: operationIdDelLote,
      expected_rev: head,
      ops: construidas.map((construida) => construida.op),
    };

    // Persistir el estado ANTES de mandar: un crash entre commit y ack deja las entradas
    // en `sent` y el reintento reusa la MISMA operación (idempotente).
    for (const entry of lote) await store.setOutboxState(entry.operationId, 'sent');
    const tCommit = performance.now();
    const ack = await deps.conn.commit(batch);
    marcarFase('commit', performance.now() - tCommit);

    // Una revisión POR LOTE (no por op): el server agrupa las ops del batch bajo la misma
    // rev (el modelo del restore masivo: "comparte batch_id y revisión"). Todas las
    // entradas se confirman con `ack.rev`; el head avanza una sola vez.
    //
    // TODO el lote se confirma en UNA transacción de IndexedDB (base + índice + outbox +
    // checkpoint): antes cada nota pagaba 4 transacciones (~16,5 ms) y era el piso del T1.
    const tConfirmar = performance.now();
    const minRev = (await store.readCheckpoint().catch(() => null))?.minRev ?? null;
    await store.confirmarLote(
      lote.map((entry, i) => datosDeConfirmacion(entry, ack.rev, construidas[i]!.blobIdHex)),
      { since: ack.rev, minRev },
    );
    marcarFase('confirmar', performance.now() - tConfirmar);
    head = ack.rev;
    empujadas += lote.length;
  };

  for (const lote of enLotes(pendientes, TAMANO_LOTE)) {
    try {
      await subir(lote);
    } catch (error) {
      if (!(error instanceof SyncConflictError)) throw error;

      // Un conflicto rechaza el lote ENTERO (el server es todo-o-nada). Se aísla
      // reintentando de a una: la que conflictúa queda `pending` y el report la nombra;
      // las demás (sin conflicto) se confirman igual.
      for (const entry of lote) {
        try {
          await subir([entry]);
        } catch (fallo) {
          if (fallo instanceof SyncConflictError) {
            await store.setOutboxState(entry.operationId, 'pending');
            return {
              diff,
              encoladas,
              empujadas,
              headRev: head,
              deletesBloqueados,
              conflicto: {
                operationId: entry.operationId,
                path: entry.path,
                mensaje: fallo.message,
              },
            };
          }
          throw fallo;
        }
      }
    }
  }

  // Desglose por fase SOLO cuando corre en una página (el runtime): el puente lo imprime.
  if (typeof location !== 'undefined') {
    marcarFase('total', performance.now() - t0Total);
    const resumen = Object.entries(FASES)
      .map(([clave, valor]) => `${clave}=${Math.round(valor)}`)
      .join(' ');
    void fetch('/fs/log', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ msg: `fases push: ${resumen}ms` }),
    }).catch(() => {
      /* el diagnóstico nunca rompe el sync */
    });
  }
  return { diff, encoladas, empujadas, headRev: head, deletesBloqueados };
}

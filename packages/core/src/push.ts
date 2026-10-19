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
import type { OutboxEntry } from './StateStore.js';

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
 * Ops por batch. El server acepta hasta MAX_BATCH_OPS (1000); 50 mantiene los lotes
 * chicos (un conflicto aisla menos trabajo) sin perder la ganancia de round-trips.
 */
const TAMANO_LOTE = 50;

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
  const entradas = await escanearVault(port, keys, '', cache);
  const diffBase = await diffLocal(store, entradas);

  // Un cliente VACÍO no borra el remoto (aceptación de fase 3): sin un solo archivo en
  // disco, los deletes del diff no son una intención — se bloquean y se informan.
  const deletesBloqueados = entradas.length === 0 ? diffBase.borrados.length : 0;
  const diff = {
    ...diffBase,
    borrados: deletesBloqueados > 0 ? [] : diffBase.borrados,
  };
  let encoladas = 0;

  const encolar = async (entry: OutboxEntry): Promise<void> => {
    // Idempotencia por payload: el MISMO cambio no duplica la operación.
    if (await store.getOutbox(entry.operationId)) return;
    await store.putOutbox(entry);
    encoladas += 1;
  };

  for (const archivo of diff.nuevos) {
    const bytes = await port.read(archivo.path);
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
    const bytes = await port.read(archivo.path);
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
    const almacenado = encryptBlob(keys, plano);
    const idBytes = blobIdDe(almacenado);
    blobIdHex = toHex(idBytes);
    blobIdBytes = idBytes;
    tamano = plano.length;
    await conn.putBlob(blobIdHex, almacenado);
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

/** Con el ack en mano: base + índice avanzan y la operación se confirma y retira. */
async function confirmar(
  deps: PushDeps,
  entry: OutboxEntry,
  rev: number,
  blobIdHex: string | null,
): Promise<void> {
  const { store } = deps;
  const esDel = entry.kind === 'del';

  await store.putBase({
    pathId: entry.pathId,
    rev,
    blobId: esDel ? null : blobIdHex,
    contentHash: esDel ? null : (entry.snapshotHash ?? null),
  });

  const indice = await store.getIndex(entry.pathId);
  await store.putIndex({
    pathId: entry.pathId,
    path: entry.path ?? indice?.path,
    kind: 'file',
    deleted: esDel,
    rev,
    blobId: esDel ? null : blobIdHex,
    size: entry.snapshot ? entry.snapshot.size : null,
    // El mtime REAL del archivo (no el de encolado): es lo que compara el escaneo
    // incremental para decidir si puede reusar el hash sin leer el contenido.
    mtime: entry.mtimeMs ?? entry.queuedAt,
    contentHash: esDel ? null : (entry.snapshotHash ?? null),
  });

  await store.setOutboxState(entry.operationId, 'confirmed');
  await store.deleteOutbox(entry.operationId);

  // La revisión quedó reconciliada: el disco YA la refleja (la subimos nosotros) y el CAS
  // garantiza que no salteamos revisiones ajenas (si otro device hubiera commiteado entre
  // medio, el ack habría sido un conflicto). El checkpoint avanza acá TAMBIÉN: si solo
  // avanzara por `completeApply`, el próximo push usaría una base vieja y chocaría contra
  // nuestras PROPIAS revisiones.
  const minRev = (await store.readCheckpoint().catch(() => null))?.minRev ?? null;
  await store.writeCheckpoint({ since: rev, minRev, updatedAt: Date.now() });
}

/** Una pasada de subida: escanear → encolar → drenar. No reconecta (eso es del llamador). */
export async function pushOnce(deps: PushDeps): Promise<PushReport> {
  const { store } = deps;
  let head = (await store.readCheckpoint().catch(() => null))?.since ?? 0;

  const { diff, encoladas, deletesBloqueados } = await encolarDiff(deps, head);
  let empujadas = 0;

  const pendientes = (await store.listOutbox())
    .filter((entry) => entry.state !== 'confirmed')
    .sort((a, b) => a.queuedAt - b.queuedAt);

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
    const construidas = await Promise.all(
      lote.map((entry) => construirOp(deps, entry, head, operacionDelLote)),
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
    const ack = await deps.conn.commit(batch);

    // Una revisión POR LOTE (no por op): el server agrupa las ops del batch bajo la misma
    // rev (el modelo del restore masivo: "comparte batch_id y revisión"). Todas las
    // entradas se confirman con `ack.rev`; el head avanza una sola vez.
    for (let i = 0; i < lote.length; i += 1) {
      await confirmar(deps, lote[i]!, ack.rev, construidas[i]!.blobIdHex);
    }
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

  return { diff, encoladas, empujadas, headRev: head, deletesBloqueados };
}

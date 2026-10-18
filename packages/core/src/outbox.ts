/**
 * Outbox **save-first**: la operación local se persiste ANTES de tocar el archivo visible.
 *
 * Invariantes (plan de fase 2, paso 2):
 *  - El `operation_id` es **determinista por payload**: reintento de un payload idéntico ⇒
 *    mismo ID (idempotente); rebase con contenido o precondición nuevos ⇒ payload nuevo ⇒
 *    operación NUEVA. Nunca se "regenera" el ID de una operación viva.
 *  - El snapshot del contenido es **recuperable** (Blob + hash verificado): el archivo del
 *    vault puede cambiar o desaparecer después — la outbox no depende de él.
 *  - Guardar solo el nombre no alcanza; guardar el contenido sin verificar su hash tampoco:
 *    si el hash declarado no coincide, se rechaza el encolado.
 */

import { bytesToUuid, cryptoProvider } from '@ovaultd/crypto';
import { b64Decode, textEncoder, toHex } from '@ovaultd/protocol';

import type { OutboxEntry } from './StateStore.js';

export interface EnqueueRequest {
  kind: 'add' | 'mod' | 'del' | 'move';
  /** Path (texto en claro, para diagnóstico y rebase). */
  path: string;
  /** `path_id` en hex (identidad de la entrada). */
  pathId: string;
  /** Solo en `move`: `path_id` de origen. */
  prevPathId?: string | null;
  /** Revisión contra la que se hará compare-and-swap. */
  expectedRev: number;
  folder?: boolean;
  /** Snapshot recuperable del contenido, en base64 (obligatorio en add/mod de archivos). */
  contentB64?: string | null;
  /** sha256 hex del contenido (obligatorio si hay `contentB64`). */
  contentHash?: string | null;
  /** Milisegundos de encolado (default: ahora). */
  queuedAt?: number;
}

/** Payload canónico del que deriva la identidad de la operación. */
const canonicalPayload = (req: EnqueueRequest): string =>
  JSON.stringify({
    kind: req.kind,
    path: req.path,
    pathId: req.pathId,
    prevPathId: req.prevPathId ?? null,
    folder: req.folder === true,
    expectedRev: req.expectedRev,
    contentHash: req.contentHash ?? null,
  });

/** `operation_id` determinista por payload (16 B → UUID textual). */
export function operationIdFor(req: EnqueueRequest): string {
  const digest = cryptoProvider().sha256(textEncoder.encode(canonicalPayload(req)));
  return bytesToUuid(digest.subarray(0, 16));
}

/** Valida y materializa una entrada de outbox. No toca el store (eso es del commit). */
export function prepareOutboxEntry(req: EnqueueRequest): OutboxEntry {
  if (!Number.isInteger(req.expectedRev) || req.expectedRev < 0) {
    throw new Error(`expectedRev inválido: ${String(req.expectedRev)}`);
  }
  if (req.pathId.length === 0 || req.path.length === 0) {
    throw new Error('path y pathId son obligatorios');
  }

  const esArchivo = req.folder !== true;
  if (req.kind === 'add' || req.kind === 'mod') {
    // La comparación es contra `undefined`/`null` A PROPÓSITO: `contentB64 === ""` es un
    // archivo vacío, que es contenido legítimo. Con un chequeo de truthiness, un archivo de
    // 0 bytes (un .md en blanco) hacía fallar el encolado y abortaba el ciclo completo.
    if (esArchivo && (req.contentB64 == null || req.contentHash == null)) {
      throw new Error(`outbox ${req.kind}: un archivo necesita contentB64 y contentHash (snapshot recuperable)`);
    }
  }
  if (req.kind === 'del' && (req.contentB64 != null || req.contentHash != null)) {
    throw new Error('outbox del: no lleva contenido');
  }
  if (req.kind === 'move' && !req.prevPathId) {
    throw new Error('outbox move: falta prevPathId');
  }

  let snapshot: Blob | null = null;
  if (req.contentB64 != null) {
    const bytes = b64Decode(req.contentB64);
    const real = toHex(cryptoProvider().sha256(bytes));
    if (real !== req.contentHash) {
      throw new Error(`el hash declarado no coincide con el contenido (declarado ${req.contentHash}, real ${real})`);
    }
    // Copia nueva (ArrayBuffer puro): Uint8Array<ArrayBufferLike> no es BlobPart válido en TS.
    snapshot = new Blob([new Uint8Array(bytes).buffer]);
  }

  return {
    operationId: operationIdFor(req),
    kind: req.kind,
    pathId: req.pathId,
    path: req.path,
    prevPathId: req.prevPathId ?? undefined,
    expectedRev: req.expectedRev,
    queuedAt: req.queuedAt ?? Date.now(),
    snapshot,
    snapshotHash: req.contentHash ?? null,
    state: 'pending',
  };
}

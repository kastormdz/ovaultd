/**
 * Journal de aplicación (IndexedDB↔filesystem) — el lado del NÚCLEO.
 *
 * La secuencia completa del plan (paso 3) es:
 *   intención persistida → descarga a temporal → verificar firmas/hash/AEAD →
 *   publicar el archivo → **commit atómico** (intención cerrada + índice/base/checkpoint).
 *
 * Este módulo cubre la parte del núcleo: la intención vive ANTES de publicar, y el avance
 * de `since` solo ocurre en el commit atómico del `StateStore.completeApply()`. La parte de
 * filesystem (temporal, verificación, publicación, detección de tercera versión) vive en el
 * `NodeFileAdapter` y se orquesta desde el CLI.
 *
 * Regla dura: `since` avanza SOLO acá. Recibir un frame no equivale a aplicarlo.
 */

import { bytesToUuid, cryptoProvider } from '@ovaultd/crypto';
import { textEncoder } from '@ovaultd/protocol';

import type { IntentEntry } from './StateStore.js';

export interface ApplyCandidate {
  /** `path_id` en hex. */
  pathId: string;
  /** Revisión que se está aplicando. */
  rev: number;
  /** `blob_id` en hex del contenido a publicar. */
  blobId: string;
  /** Ruta relativa del destino (para la recuperación). */
  path?: string;
  kind?: 'file' | 'folder';
  deleted?: boolean;
  contentHash?: string | null;
  /** Nombre del archivo temporal en el vault (relativo), si ya se eligió. */
  tempName?: string;
}

/** `intentId` determinista por candidato: repetir el begin de la misma revisión no duplica. */
export function intentIdFor(candidate: ApplyCandidate): string {
  const digest = cryptoProvider().sha256(
    textEncoder.encode(`apply:${candidate.pathId}:${candidate.rev}:${candidate.blobId}`),
  );
  return bytesToUuid(digest.subarray(0, 16));
}

/** Valida y materializa una intención de aplicación. */
export function createIntent(candidate: ApplyCandidate): IntentEntry {
  if (!Number.isInteger(candidate.rev) || candidate.rev < 0) {
    throw new Error(`rev inválida: ${String(candidate.rev)}`);
  }
  if (candidate.pathId.length === 0) {
    throw new Error('pathId es obligatorio');
  }
  // Una carpeta no tiene blob: el guard la trata aparte y lo único que se publica es el
  // `mkdirp`. Antes esto explotaba con "blobId es obligatorio salvo en un borrado" y una
  // revisión `folder` nueva cortaba el ciclo con una excepción.
  const esCarpeta = candidate.kind === 'folder' && candidate.deleted !== true;
  if (candidate.blobId.length === 0 && candidate.deleted !== true && !esCarpeta) {
    throw new Error('blobId es obligatorio salvo en un borrado o una carpeta');
  }
  return {
    intentId: intentIdFor(candidate),
    pathId: candidate.pathId,
    rev: candidate.rev,
    blobId: candidate.blobId,
    state: 'pending',
    path: candidate.path,
    kind: candidate.kind,
    deleted: candidate.deleted ?? false,
    contentHash: candidate.contentHash ?? null,
    tempName: candidate.tempName,
    createdAt: Date.now(),
  };
}

/**
 * Reconcile — el ciclo de convergencia (lado PULL de fase 2; el drenaje de la outbox
 * llega con el comando `sync` del CLI).
 *
 * Reglas:
 *  - `since` avanza SOLO por `completeApply` (aplicación recuperable) — acá no se toca.
 *  - Una revisión `blocked` CORTA el ciclo: el checkpoint no puede saltar una revisión
 *    sin aplicar, así que nada posterior se aplica hasta resolverla (fase 5).
 *  - `min_rev` mayor que `since` ⇒ **resync**: se informa SIN borrar nada — la outbox
 *    y el estado local se preservan (jamás "arreglar" el cursor borrando estado).
 */

import { decryptPathEnc } from '@ovaultd/crypto';
import { b64Decode, fromHex, toHex, type SignedRecordWire } from '@ovaultd/protocol';

import { applyRevision, type ApplyDeps, type ApplyRevision } from './apply-runner.js';
import type { SyncConnection } from './SyncConnection.js';

export interface PullReport {
  status: 'pulled' | 'blocked' | 'resync-required';
  applied: number;
  headRev: number;
  blocked?: { rev: number; pathId: string; reason?: string };
  resync?: { since: number; minRev: number };
}

const CERO_32 = '00'.repeat(32);

/** ¿El `blob_id` de la delta es el centinela de "sin blob" (carpeta)? */
const sinBlob = (blobIdB64: string): boolean => {
  try {
    return toHex(b64Decode(blobIdB64)) === CERO_32;
  } catch {
    return false;
  }
};

async function aplicarRegistro(
  deps: ApplyDeps & { forzar?: boolean },
  conn: SyncConnection,
  registro: SignedRecordWire,
): Promise<{ aplicado: boolean; bloqueado?: { reason?: string } }> {
  const { store, keys } = deps;
  const pathIdHex = toHex(b64Decode(registro.path_id));
  const deleted = registro.op === 'del';
  const folder = !deleted && sinBlob(registro.blob_id);

  // El `path` sale del path_enc (registros con contenido) o del índice local (tombstones:
  // la delta no manda path_enc en `del`). Un borrado de algo que nunca vimos no toca el vault.
  let path = '';
  if (!deleted && registro.path_enc) {
    path = decryptPathEnc(keys, b64Decode(registro.path_enc), fromHex(pathIdHex));
  } else if (deleted) {
    path = (await store.getIndex(pathIdHex))?.path ?? '';
  }

  const revision: ApplyRevision = {
    pathId: pathIdHex,
    path,
    kind: folder ? 'folder' : 'file',
    deleted,
    rev: registro.rev,
    blobId: deleted || folder ? null : toHex(b64Decode(registro.blob_id)),
    // La delta no trae content_hash: la integridad la da el AEAD del blob (ADR-0014).
    contentHash: null,
  };

  let stored: Uint8Array | null = null;
  if (!deleted && !folder) {
    stored = await conn.getBlob(revision.blobId!);
  }

  // La conexión viaja en los deps: es lo que le permite al apply intentar la fusión
  // 3-vías (bajar el blob de la base del CAS) cuando el guard bloquea por "ambos cambiaron".
  const resultado = await applyRevision({ ...deps, conn }, revision, stored, {
    forzar: deps.forzar === true,
  });
  if (resultado.status === 'blocked') {
    return { aplicado: false, bloqueado: { reason: resultado.reason } };
  }
  return { aplicado: true };
}

/** Una pasada de convergencia (pull). No reconecta: eso es de `retry`/llamador. */
export async function pullOnce(
  conn: SyncConnection,
  deps: ApplyDeps & { forzar?: boolean },
): Promise<PullReport> {
  const checkpoint = await deps.store.readCheckpoint().catch(() => null);
  let cursor = checkpoint?.since ?? 0;
  let applied = 0;

  for (;;) {
    const cambios = await conn.pull(cursor);

    if (cambios.min_rev != null && cursor < cambios.min_rev) {
      return {
        status: 'resync-required',
        applied,
        headRev: cursor,
        resync: { since: cursor, minRev: cambios.min_rev },
      };
    }

    for (const registro of cambios.items) {
      const resultado = await aplicarRegistro(deps, conn, registro);
      if (resultado.bloqueado) {
        return {
          status: 'blocked',
          applied,
          headRev: cursor,
          blocked: { rev: registro.rev, pathId: toHex(b64Decode(registro.path_id)), reason: resultado.bloqueado.reason },
        };
      }
      applied += 1;
    }

    const siguiente = cambios.next_since;
    if (siguiente <= cursor) break; // la página no avanzó: no hay más
    cursor = siguiente;
  }

  return { status: 'pulled', applied, headRev: cursor };
}

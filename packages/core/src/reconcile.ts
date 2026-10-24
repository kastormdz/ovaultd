/**
 * Reconcile — el ciclo de convergencia (lado PULL de fase 2; el drenaje de la outbox
 * llega con el comando `sync` del CLI).
 *
 * Reglas:
 *  - `since` avanza SOLO acá, al cerrar una revisión COMPLETA (aplicación recuperable).
 *    Una revisión del server trae N operaciones: escribir el cursor al aplicar la primera
 *    dejaba las demás inalcanzables para siempre (`rev > since`).
 *  - Un `blocked` NO corta el ciclo (C-a, 0.2.21): el intent queda `blocked` (visible y
 *    resoluble desde el Resolver) y el cursor avanza igual. Antes una sola carpeta en
 *    conflicto dejaba al device sin bajar NADA más, para siempre.
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
  /** Primer bloqueo de la pasada (compat: el detalle completo está en `bloqueados`). */
  blocked?: { rev: number; pathId: string; reason?: string };
  /** Todos los bloqueos de la pasada: ya no frenan al resto del vault. */
  bloqueados?: Array<{ rev: number; pathId: string; reason?: string }>;
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
  let minRev = checkpoint?.minRev ?? null;
  let applied = 0;
  const bloqueados: Array<{ rev: number; pathId: string; reason?: string }> = [];
  const cursorInicial = cursor;

  for (;;) {
    const antes = cursor;
    const cambios = await conn.pull(cursor);

    if (cambios.min_rev != null && cursor < cambios.min_rev) {
      return {
        status: 'resync-required',
        applied,
        headRev: cursor,
        resync: { since: cursor, minRev: cambios.min_rev },
      };
    }

    if (cambios.min_rev != null) minRev = cambios.min_rev;

    for (let i = 0; i < cambios.items.length; i += 1) {
      const registro = cambios.items[i]!;
      const resultado = await aplicarRegistro(deps, conn, registro);
      if (resultado.bloqueado) {
        // El intent ya quedó `blocked` en el store (lo escribió el apply): acá sólo se
        // registra y se SIGUE. La operación no se reintenta sola; el resto del vault sí baja.
        bloqueados.push({
          rev: registro.rev,
          pathId: toHex(b64Decode(registro.path_id)),
          reason: resultado.bloqueado.reason,
        });
      } else {
        applied += 1;
      }

      // ¿Se cerró la revisión? El server pagina por revisión entera, así que el cambio de
      // rev (o el fin de la página) marca que ya no queda ninguna op de esa rev sin aplicar.
      const siguienteRegistro = cambios.items[i + 1];
      if (!siguienteRegistro || siguienteRegistro.rev !== registro.rev) {
        if (registro.rev > cursor) {
          cursor = registro.rev;
          await deps.store.writeCheckpoint({ since: cursor, minRev });
        }
      }
    }

    // Página sin operaciones (o con menos de las que el server ya consideró vistas): el
    // server garantiza que hasta `next_since` no queda nada pendiente.
    if (cambios.next_since > cursor) {
      cursor = cambios.next_since;
      await deps.store.writeCheckpoint({ since: cursor, minRev });
    }

    if (cambios.next_since <= antes || cambios.items.length === 0) break;
  }

  // Con bloqueos pero sin avance (el único ítem era el conflicto) el estado sigue siendo
  // `blocked` para quien mira sólo el status; si algo avanzó, el ciclo convergió igual.
  const status: PullReport['status'] =
    bloqueados.length > 0 && cursor === cursorInicial ? 'blocked' : 'pulled';
  return {
    status,
    applied,
    headRev: cursor,
    ...(bloqueados.length > 0 ? { blocked: bloqueados[0]!, bloqueados } : {}),
  };
}

/**
 * SyncClient — el orquestador del ciclo de convergencia (fase 3, paso 3).
 *
 * La tabla de decisión del plan, contra la base:
 *  - local ≠ base y remoto = base  → **push** (lo maneja el drenado de la outbox).
 *  - remoto ≠ base y local = base  → **pull** (apply; el guard de tercera versión decide).
 *  - ambos = base                  → **convergencia** (no pasa nada).
 *  - ambos ≠ base                  → **conflicto EXPLÍCITO**: el pull deja la revisión
 *    `blocked` con el motivo y el ciclo lo reporta. Resolverlo (merge conservador o
 *    copia) es el corte del merge; hasta entonces, un conflicto se VE, no se adivina.
 *
 * Orden del ciclo: **pull → push** (plan §4.2). El pull deja la base al día y el push
 * rebasea contra el head actual. Si el pull quedó `blocked` o pidió `resync`, el ciclo
 * NO pushea: el checkpoint quedó atrás de una revisión sin aplicar y el CAS rechazaría
 * todo lo nuestro — mejor UN conflicto visible que N conflicts ruidosos. Los cambios
 * locales esperan en la outbox (save-first): nada se pierde.
 */

import type { FileSystemPort } from './file-port.js';
import { pushOnce, type PushReport } from './push.js';
import { pullOnce, type PullReport } from './reconcile.js';
import type { SyncConnection } from './SyncConnection.js';
import type { StateStore } from './StateStore.js';
import type { SubKeys } from '@ovaultd/crypto';

export interface SyncCycleDeps {
  store: StateStore;
  port: FileSystemPort;
  keys: SubKeys;
  conn: SyncConnection;
  /** `vault_id` de 16 B del material (el que firma), no el numérico del server. */
  vaultUuid: Uint8Array;
  epoch: number;
}

export interface ConflictoDeSync {
  tipo: 'pull-bloqueado' | 'push-conflicto' | 'resync';
  pathId?: string;
  rev?: number;
  mensaje: string;
}

export interface SyncCycleReport {
  /** `convergido` = sin conflictos (haya habido cambios o no). */
  estado: 'convergido' | 'con-conflictos' | 'resync-requerido';
  pull: PullReport;
  /** `null` cuando el ciclo no llegó a pushear (pull bloqueado o resync). */
  push: PushReport | null;
  conflictos: ConflictoDeSync[];
}

/** Una pasada completa: pull y (si se puede) push. No reconecta: eso es de `retry`. */
export async function correrCiclo(deps: SyncCycleDeps): Promise<SyncCycleReport> {
  const { store, port, keys, conn, vaultUuid, epoch } = deps;

  const pull = await pullOnce(conn, { store, port, keys });

  if (pull.status === 'resync-required') {
    const detalle = pull.resync ?? { since: -1, minRev: -1 };
    return {
      estado: 'resync-requerido',
      pull,
      push: null,
      conflictos: [
        {
          tipo: 'resync',
          mensaje: `resync requerido: since ${detalle.since} < min_rev ${detalle.minRev} (fase 4)`,
        },
      ],
    };
  }

  if (pull.status === 'blocked') {
    const bloqueado = pull.blocked!;
    return {
      estado: 'con-conflictos',
      pull,
      push: null,
      conflictos: [
        {
          tipo: 'pull-bloqueado',
          pathId: bloqueado.pathId,
          rev: bloqueado.rev,
          mensaje: bloqueado.reason ?? 'conflicto sin detalle',
        },
      ],
    };
  }

  const push = await pushOnce({ store, port, keys, conn, vaultUuid, epoch });

  const conflictos: ConflictoDeSync[] = [];
  if (push.conflicto !== undefined) {
    conflictos.push({
      tipo: 'push-conflicto',
      mensaje: push.conflicto.path
        ? `${push.conflicto.mensaje} (ruta: ${push.conflicto.path})`
        : push.conflicto.mensaje,
    });
  }

  return {
    estado: conflictos.length > 0 ? 'con-conflictos' : 'convergido',
    pull,
    push,
    conflictos,
  };
}

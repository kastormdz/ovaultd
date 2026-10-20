/**
 * prune — reconciliación de los paths vivos del server contra el disco local.
 *
 * El server es ciego (E2EE): no puede listar paths ni ver el filesystem. Un
 * borrado local que nunca pasó por el sync (a mano, con el cliente apagado, o
 * en un vault viejo) deja el path vivo en `files` para siempre, y el panel lo
 * cuenta como nota. Este módulo baja el oplog COMPLETO, lo colapsa por path_id
 * (gana la última op; un `del` lo saca) y compara cada ruta viva contra el
 * disco. Por defecto es una LECTURA: solo `aplicarPrune` encola los `del`.
 */

import { decryptPathEnc, pathId as calcularPathId, type SubKeys } from '@ovaultd/crypto';
import { b64Decode, toHex } from '@ovaultd/protocol';

import type { FileSystemPort } from './file-port.js';
import { prepareOutboxEntry } from './outbox.js';
import type { StateStore } from './StateStore.js';
import type { SyncConnection } from './SyncConnection.js';
import { validarRutaDeVault } from './vault-path.js';

interface EntradaViva {
  blobId: string;
  pathEnc: string;
}

const CERO_32 = '00'.repeat(32);

/** ¿El `blob_id` es el centinela de "sin blob" (carpeta)? */
const esCarpeta = (blobIdB64: string): boolean => {
  try {
    return toHex(b64Decode(blobIdB64)) === CERO_32;
  } catch {
    return false;
  }
};

/** Trae el delta COMPLETO (paginado) y lo colapsa al estado vivo por `path_id`. */
export async function estadoVivoDelServer(conn: SyncConnection): Promise<Map<string, EntradaViva>> {
  const vivos = new Map<string, EntradaViva>();
  let cursor = 0;
  for (;;) {
    const pagina = await conn.pull(cursor, 500);
    for (const item of pagina.items) {
      if (item.op === 'del') {
        vivos.delete(item.path_id);
        continue;
      }
      if (esCarpeta(item.blob_id)) continue;
      vivos.set(item.path_id, { blobId: item.blob_id, pathEnc: item.path_enc });
    }
    const siguiente = pagina.next_since;
    if (siguiente <= cursor) break;
    cursor = siguiente;
  }
  return vivos;
}

export interface PlanDePrune {
  /** Paths vivos según el server. */
  vivos: number;
  /** Vivos que SÍ existen en el disco local. */
  enDisco: number;
  /** Vivos que no existen en el disco: los que el panel cuenta de más. */
  huerfanos: string[];
  /** `path_enc` que no descifra con estas claves. */
  ilegibles: number;
  /** Rutas que la política portable rechaza (nunca se borran a ciegas). */
  bloqueadas: number;
}

export interface DepsPrune {
  conn: SyncConnection;
  keys: SubKeys;
  port: FileSystemPort;
}

export async function planificarPrune(deps: DepsPrune): Promise<PlanDePrune> {
  const vivos = await estadoVivoDelServer(deps.conn);
  const plan: PlanDePrune = {
    vivos: vivos.size,
    enDisco: 0,
    huerfanos: [],
    ilegibles: 0,
    bloqueadas: 0,
  };

  for (const [pathIdB64, entrada] of vivos) {
    let ruta: string;
    try {
      ruta = decryptPathEnc(deps.keys, b64Decode(entrada.pathEnc), b64Decode(pathIdB64));
    } catch {
      plan.ilegibles += 1;
      continue;
    }
    // Una ruta hostil no se borra ni se reporta como huérfana: se bloquea y se
    // cuenta (misma política que el apply/export).
    if (!validarRutaDeVault(ruta).ok) {
      plan.bloqueadas += 1;
      continue;
    }
    const stat = await deps.port.stat(ruta);
    if (stat === null) plan.huerfanos.push(ruta);
    else plan.enDisco += 1;
  }

  plan.huerfanos.sort();
  return plan;
}

export interface ResultadoPrune extends PlanDePrune {
  /** Cuántos `del` quedaron encolados (0 si no se aplicó o no había huérfanos). */
  encolados: number;
}

/**
 * Encola un `del` por cada huérfano. No publica: el que llama corre el push
 * normal después (`pushOnce`), para que el lote salga con la firma y el
 * `expected_rev` de siempre.
 */
export async function aplicarPrune(deps: DepsPrune & { store: StateStore }): Promise<ResultadoPrune> {
  const plan = await planificarPrune(deps);
  if (plan.huerfanos.length === 0) return { ...plan, encolados: 0 };

  const checkpoint = await deps.store.readCheckpoint().catch(() => null);
  const expectedRev = checkpoint?.since ?? 0;
  const entradas = plan.huerfanos.map((ruta) =>
    prepareOutboxEntry({
      kind: 'del',
      path: ruta,
      pathId: toHex(calcularPathId(deps.keys, ruta)),
      expectedRev,
    }),
  );
  await deps.store.putOutboxMany(entradas);
  return { ...plan, encolados: entradas.length };
}

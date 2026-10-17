/**
 * Export — baja el HEAD completo de un vault y lo deja escrito y descifrado.
 *
 * A diferencia del pull (que aplica deltas al vault y mueve el checkpoint), el export es
 * una LECTURA pura: no toca el estado de sync y escribe donde el puerto le diga (fs del
 * SO en el plugin de escritorio, un directorio del CLI, memoria en los tests).
 *
 * Reglas:
 *  - El delta se pide desde `since = 0` y se COLPASA por `path_id`: gana la última
 *    revisión viva de cada ruta; un `del` la saca. Las carpetas no se materializan
 *    (los archivos crean sus padres; una carpeta vacía no existe en el export).
 *  - Cada ruta descifrada se VALIDA antes de escribir (misma política portable que el
 *    apply): una ruta hostil del server no puede escapar del destino.
 *  - Con `omitirExistentes` (default), lo que ya está en el destino no se pisa.
 */

import { decryptBlob, decryptPathEnc, type SubKeys } from '@ovaultd/crypto';
import { b64Decode, toHex } from '@ovaultd/protocol';

import type { SyncConnection } from './SyncConnection.js';
import { validarRutaDeVault } from './vault-path.js';

/** El destino del export. El plugin lo implementa con fs (escritorio); el CLI con node:fs. */
export interface PuertoDeExport {
  /** Escribe `contenido` en `ruta` (relativa y ya validada), creando los padres. */
  escribir(ruta: string, contenido: Uint8Array): Promise<void>;
  /** ¿Ya existe `ruta` en el destino? */
  existe(ruta: string): Promise<boolean>;
}

export interface ResultadoExport {
  /** Archivos escritos (nuevos, o reescritos si `omitirExistentes` es false). */
  escritas: number;
  /** Archivos que ya existían y se omitieron. */
  omitidas: number;
  /** Rutas del server que la política portable rechazó: NUNCA se escriben. */
  bloqueadas: number;
  /** Bytes en claro escritos. */
  bytes: number;
}

export interface OpcionesExport {
  conn: SyncConnection;
  keys: SubKeys;
  puerto: PuertoDeExport;
  /** `true` (default): no reescribe un archivo que ya existe en el destino. */
  omitirExistentes?: boolean;
  onProgreso?: (hechos: number, total: number) => void;
}

const CERO_32 = '00'.repeat(32);

/** ¿El `blob_id` del registro es el centinela de "sin blob" (carpeta)? */
const esCarpeta = (blobIdB64: string): boolean => {
  try {
    return toHex(b64Decode(blobIdB64)) === CERO_32;
  } catch {
    return false;
  }
};

interface EntradaViva {
  blobId: string;
  pathEnc: string;
}

/** Trae el delta COMPLETO (paginado) y lo colapsa al estado vivo por `path_id`. */
async function estadoVivo(conn: SyncConnection): Promise<Map<string, EntradaViva>> {
  const vivos = new Map<string, EntradaViva>();
  let cursor = 0;

  for (;;) {
    const pagina = await conn.pull(cursor, 500);
    for (const item of pagina.items) {
      if (item.op === 'del') {
        vivos.delete(item.path_id);
        continue;
      }
      // Carpetas: no se materializan (los archivos crean sus padres).
      if (esCarpeta(item.blob_id)) continue;
      vivos.set(item.path_id, { blobId: item.blob_id, pathEnc: item.path_enc });
    }

    const siguiente = pagina.next_since;
    if (siguiente <= cursor) break; // la página no avanzó: no hay más
    cursor = siguiente;
  }

  return vivos;
}

export async function exportarVault(opciones: OpcionesExport): Promise<ResultadoExport> {
  const { conn, keys, puerto } = opciones;
  const omitirExistentes = opciones.omitirExistentes ?? true;

  const vivos = await estadoVivo(conn);
  const total = vivos.size;
  const resultado: ResultadoExport = { escritas: 0, omitidas: 0, bloqueadas: 0, bytes: 0 };
  let hechos = 0;

  for (const [pathIdB64, entrada] of vivos) {
    hechos += 1;

    // 1) Descifrar la ruta. Un path_enc que no autentica se cuenta como bloqueada.
    let ruta: string;
    try {
      ruta = decryptPathEnc(keys, b64Decode(entrada.pathEnc), b64Decode(pathIdB64));
    } catch {
      resultado.bloqueadas += 1;
      opciones.onProgreso?.(hechos, total);
      continue;
    }

    // 2) La ruta DESCIFRADA se valida ANTES de tocar el disco (traversal, absolutas,
    //    caracteres que otra plataforma no representa): misma política que el apply.
    const validez = validarRutaDeVault(ruta);
    if (!validez.ok) {
      resultado.bloqueadas += 1;
      opciones.onProgreso?.(hechos, total);
      continue;
    }

    if (omitirExistentes && (await puerto.existe(ruta))) {
      resultado.omitidas += 1;
      opciones.onProgreso?.(hechos, total);
      continue;
    }

    // 3) Blob → descifrado → escrito.
    const blobIdHex = toHex(b64Decode(entrada.blobId));
    const contenido = decryptBlob(keys, await conn.getBlob(blobIdHex));
    await puerto.escribir(ruta, contenido);

    resultado.escritas += 1;
    resultado.bytes += contenido.length;
    opciones.onProgreso?.(hechos, total);
  }

  return resultado;
}

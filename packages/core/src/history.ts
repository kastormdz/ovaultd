/**
 * Historia y preview — LECTURA pura del oplog de un archivo (fase 5, corte 5b).
 *
 * Reglas:
 *  - No escribe ni toca el estado de sync: la conexión solo se usa para leer. El
 *    llamador decide qué hacer con el contenido (mostrarlo, compararlo).
 *  - El server no descifra: acá se resuelven los nombres (`path_enc` + la path key
 *    del vault) y el contenido. El server solo ve HMACs, hex y base64.
 *  - La versión elegida se VERIFICA (sha256 del blob descargado contra su `blob_id`)
 *    antes de descifrar: un server que manda otra cosa se cae acá, no en tu vault.
 *  - Funciona también para borrados (`del` sin blob) y nombres viejos: la cadena de
 *    renames la resuelve el server (`prev_path_id`), y cada item conserva el nombre
 *    que tenía ENTONCES.
 */

import { decryptBlob, decryptPathEnc, blobId as calcularBlobId, pathId, type SubKeys } from '@ovaultd/crypto';
import { b64Decode, fromHex, toHex } from '@ovaultd/protocol';
import DiffMatchPatch from 'diff-match-patch';

import type { SyncConnection } from './SyncConnection.js';

/** Una revisión de la historia, con el nombre descifrado cuando el registro lo trae. */
export interface RevisionResumen {
  rev: number;
  op: 'add' | 'mod' | 'del' | 'move';
  /** `path_id` en hex (el HMAC que identifica la ruta en el server). */
  pathId: string;
  /** Nombre de la ruta ENTONCES, descifrado; `null` si el registro no trae `path_enc` (p. ej. un `del`). */
  ruta: string | null;
  /** `blob_id` en hex; `null` en `del` y carpetas. */
  blobId: string | null;
  size: number | null;
  deviceId: number | null;
  epoch: number;
  /** Timestamp del server (UTC, `YYYY-MM-DD HH:MM:SS`). */
  ts: string;
}

export interface ResultadoHistoria {
  items: RevisionResumen[];
  /** `before` para seguir paginando hacia atrás; `null` = no hay más. */
  nextBefore: number | null;
  /** Piso de retención del vault: por debajo de esto la historia ya no está. */
  minRev: number;
}

export interface OpcionesHistoria {
  conn: SyncConnection;
  keys: SubKeys;
  /** Ruta relativa dentro del vault (p. ej. `Notas/Proyecto.md`). */
  ruta: string;
  /** Acota a `rev < before`. */
  before?: number;
  limit?: number;
}

/** Pide la historia de un archivo y descifra los nombres de cada revisión. */
export async function historiaDeArchivo(opciones: OpcionesHistoria): Promise<ResultadoHistoria> {
  const { conn, keys, ruta } = opciones;

  const pathIdHex = toHex(pathId(keys, ruta));
  const pagina = await conn.historial(pathIdHex, opciones.before, opciones.limit ?? 50);

  const items: RevisionResumen[] = pagina.items.map((item) => {
    let nombre: string | null = null;
    if (item.path_enc) {
      try {
        nombre = decryptPathEnc(keys, b64Decode(item.path_enc), fromHex(item.path_id));
      } catch {
        nombre = null; // un nombre que no autentica no es un nombre
      }
    }

    return {
      rev: item.rev,
      op: item.op,
      pathId: item.path_id,
      ruta: nombre,
      blobId: item.blob_hash ?? null,
      size: item.size ?? null,
      deviceId: item.device_id ?? null,
      epoch: item.epoch,
      ts: item.ts,
    };
  });

  return { items, nextBefore: pagina.next_before, minRev: pagina.min_rev };
}

export interface OpcionesContenido {
  conn: SyncConnection;
  keys: SubKeys;
  /** `blob_id` en hex (el `blobId` de un item de la historia). */
  blobId: string;
}

/**
 * Baja y devuelve el contenido descifrado de una revisión, SIN verificar nada más que
 * la integridad: el hash del blob se recalcula antes de descifrar (un server que
 * responde otra cosa se rechaza acá) y el AEAD autentica el contenido.
 */
export async function contenidoDeRevision(opciones: OpcionesContenido): Promise<Uint8Array> {
  const { conn, keys, blobId } = opciones;

  const bytes = await conn.getBlob(blobId);
  const calculado = toHex(calcularBlobId(bytes));
  if (calculado !== blobId.toLowerCase()) {
    throw new Error(
      `el blob descargado no coincide con su hash (esperado ${blobId}, real ${calculado})`,
    );
  }

  return decryptBlob(keys, bytes);
}

/**
 * Diff legible por LÍNEAS (`-` lo que estaba, `+` lo que llega): para el preview del
 * CLI. El diff CRUDO de diff-match-patch sobre líneas, sin limpieza semántica — los
 * límites de línea son exactos y el render no miente.
 */
export function diffDeTextos(anterior: string, nuevo: string): string {
  if (anterior === nuevo) return '';

  const dmp = new DiffMatchPatch();
  const {
    chars1: charsAnterior,
    chars2: charsNuevo,
    lineArray: lineas,
  } = dmp.diff_linesToChars_(anterior, nuevo);
  const diffs = dmp.diff_main(charsAnterior, charsNuevo, false);
  dmp.diff_charsToLines_(diffs, lineas);

  const salida: string[] = [];
  for (const [operacion, texto] of diffs) {
    if (operacion === 0) continue; // sin contexto: solo lo que cambia
    const prefijo = operacion === 1 ? '+' : '-';
    const contenido = texto.endsWith('\n') ? texto.slice(0, -1) : texto;
    if (contenido === '') continue;
    for (const linea of contenido.split('\n')) {
      salida.push(`${prefijo} ${linea}`);
    }
  }

  return salida.join('\n');
}

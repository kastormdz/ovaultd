/**
 * Escaneo del vault y hash-diff contra la base sincronizada (fase 2, paso 4).
 *
 * El escaneo es solo-lectura: recorre el vault por el `FileSystemPort`, calcula
 * `path_id` y el sha256 de cada archivo, y no toca nada más. El diff compara contra la
 * base (la última revisión aplicada o empujada) y produce las operaciones candidatas.
 *
 * Decisiones:
 *  - Solo archivos: las carpetas existen implícitas en el server (el protocolo las
 *    soporta, pero el MVP no las empuja y la convergencia no las necesita).
 *  - Las exclusiones viven en `SyncFilter` (política única del núcleo): temporales del
 *    apply, configuración del vault, papelera, git anidado, node_modules. Acá solo se
 *    aplican — y "excluir no significa borrar": un path excluido que ya esté en la base
 *    NO se propone como borrado.
 *  - Sin heurística de rename en esta pasada: un rename se emite `del` + `add`, que es
 *    correcto SIEMPRE; la detección por `prev_path_id` es una optimización posterior.
 */

import { cryptoProvider, pathId as calcularPathId, type SubKeys } from '@ovaultd/crypto';
import { toHex } from '@ovaultd/protocol';

import type { FileSystemPort, VaultDirEntry, VaultStat } from './file-port.js';
import { estaExcluido } from './SyncFilter.js';
import { hashDePath } from './apariencia.js';
import type { StateStore } from './StateStore.js';

export interface EntradaEscaneada {
  path: string;
  /** `path_id` en hex. */
  pathId: string;
  /** sha256 hex del contenido. */
  contentHash: string;
  size: number;
  mtimeMs: number;
}

export interface DiffLocal {
  nuevos: EntradaEscaneada[];
  modificados: EntradaEscaneada[];
  borrados: Array<{ pathId: string; path?: string }>;
  sinCambios: number;
  /** Cambios ya encolados (outbox pendiente): el diff NO los vuelve a encolar. */
  yaEncolados: number;
}

/** Recorre el vault completo (recursivo) y devuelve los archivos con su hash. */
export async function escanearVault(
  port: FileSystemPort,
  keys: SubKeys,
  dir = '',
  cache?: Map<string, { mtime: number | null; size: number | null; contentHash: string | null }>,
  /** Si se pasa, el escaneo deja acá los bytes leídos: el push los reusa para encolar el
   *  snapshot en vez de volver a pedir cada archivo al puente (una lectura por archivo). */
  contenidos?: Map<string, Uint8Array>,
): Promise<EntradaEscaneada[]> {
  const archivos: string[] = [];

  const recorrer = async (relDir: string): Promise<void> => {
    // El puente rechaza '' (frontera estricta): la raíz del vault se pide como '.'.
    const listado = await port.list(relDir === '' ? '.' : relDir);
    for (const entrada of listado) {
      const rel = relDir === '' ? entrada.name : `${relDir}/${entrada.name}`;
      // Exclusiones PRIMERO: una carpeta excluida no se escanea NI se recursa.
      if (estaExcluido(rel)) continue;
      if (entrada.isDirectory) {
        await recorrer(rel);
        continue;
      }
      if (!entrada.isFile) continue;
      archivos.push(rel);
    }
  };
  await recorrer(dir);

  // Stat MASIVO: un solo round-trip por todo el vault (el escaneo pagaba uno por archivo
  // contra el puente: 1000 POST en el bench). Sin `statMany` en el port, bucle clásico.
  const stats = port.statMany
    ? await port.statMany(archivos)
    : await Promise.all(archivos.map((rel) => port.stat(rel)));

  const salida: EntradaEscaneada[] = [];
  const porLeer: Array<{ rel: string; stat: VaultStat }> = [];
  for (let i = 0; i < archivos.length; i += 1) {
    const rel = archivos[i]!;
    const stat = stats[i] ?? null;
    // Se fue entre el list y el stat: no hay nada que sincronizar de este path.
    if (stat === null) continue;

    // Escaneo incremental: si el archivo no cambió (mismo mtime y tamaño) el hash del
    // índice sigue valiendo y NO se lee el contenido. Un vault de 100 MB pasa de ~8 s
    // a milisegundos por ciclo. El riesgo de un mtime falseado lo cubren el watcher
    // (los cambios locales llegan por evento) y un rescan completo a demanda.
    const recordado = cache?.get(rel);
    if (
      recordado &&
      recordado.contentHash != null &&
      recordado.mtime === stat.mtimeMs &&
      recordado.size === stat.size
    ) {
      salida.push({
        path: rel,
        pathId: toHex(calcularPathId(keys, rel)),
        contentHash: recordado.contentHash,
        size: stat.size,
        mtimeMs: stat.mtimeMs,
      });
      continue;
    }

    porLeer.push({ rel, stat });
  }

  // Reads MASIVOS: los archivos nuevos o cambiados se leen en tandas (un round-trip por
  // lote de 100) en vez de uno por archivo contra el puente (~30 ms cada uno: el cuello
  // que quedaba en la primera subida). Sin `readMany` en el port, bucle clásico.
  const LOTE_LECTURA = 150;
  const leidos = new Map<string, Uint8Array>();
  if (port.readMany && porLeer.length > 0) {
    for (let i = 0; i < porLeer.length; i += LOTE_LECTURA) {
      const lote = porLeer.slice(i, i + LOTE_LECTURA);
      const blobs = await port.readMany(lote.map((pendiente) => pendiente.rel));
      for (let j = 0; j < lote.length; j += 1) {
        const bytes = blobs[j];
        if (bytes) {
          leidos.set(lote[j]!.rel, bytes);
          contenidos?.set(lote[j]!.rel, bytes);
        }
      }
    }
  } else {
    for (const { rel } of porLeer) {
      const bytes = await port.read(rel);
      leidos.set(rel, bytes);
      contenidos?.set(rel, bytes);
    }
  }

  for (const { rel, stat } of porLeer) {
    const bytes = leidos.get(rel);
    // Se fue entre el stat y el read: no se puede hashear lo que ya no está.
    if (!bytes) continue;
    salida.push({
      path: rel,
      pathId: toHex(calcularPathId(keys, rel)),
      contentHash: hashDePath(rel, bytes),
      size: bytes.length,
      mtimeMs: stat.mtimeMs,
    });
  }

  salida.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  return salida;
}

/** Diff contra la base: qué falta empujar, con la outbox pendiente como filtro. */
export async function diffLocal(store: StateStore, entradas: EntradaEscaneada[]): Promise<DiffLocal> {
  const base = new Map((await store.listBase()).map((entrada) => [entrada.pathId, entrada]));
  const index = new Map((await store.listIndex()).map((entrada) => [entrada.pathId, entrada]));
  const pendientes = new Set(
    (await store.listOutbox())
      .filter((entrada) => entrada.state !== 'confirmed')
      .map((entrada) => entrada.pathId),
  );

  const enDisco = new Set<string>();
  const diff: DiffLocal = { nuevos: [], modificados: [], borrados: [], sinCambios: 0, yaEncolados: 0 };

  for (const archivo of entradas) {
    enDisco.add(archivo.pathId);
    const baseEntry = base.get(archivo.pathId);
    if (!baseEntry || baseEntry.contentHash == null) {
      // Nunca subido (o sin hash conocido): `add`. Si hay una operación pendiente del
      // MISMO path, no se re-encola (la outbox espera su turno).
      if (pendientes.has(archivo.pathId)) {
        diff.yaEncolados += 1;
      } else {
        diff.nuevos.push(archivo);
      }
      continue;
    }
    if (baseEntry.contentHash === archivo.contentHash) {
      diff.sinCambios += 1;
      continue;
    }
    if (pendientes.has(archivo.pathId)) {
      diff.yaEncolados += 1;
    } else {
      diff.modificados.push(archivo);
    }
  }

  // Borrados: base sin archivo en disco (y que el índice no tenga ya como tombstone).
  for (const [pathIdHex, baseEntry] of base) {
    if (enDisco.has(pathIdHex) || baseEntry.contentHash == null) continue;
    const indexEntry = index.get(pathIdHex);
    if (indexEntry?.deleted === true) continue;
    // Excluir NO significa borrar el remoto: un path hoy excluido jamás se propone `del`.
    if (indexEntry?.path && estaExcluido(indexEntry.path)) continue;
    if (pendientes.has(pathIdHex)) {
      diff.yaEncolados += 1;
      continue;
    }
    diff.borrados.push({ pathId: pathIdHex, path: indexEntry?.path });
  }

  return diff;
}

/** Resumen humano del diff (lo usan `list-local` y los reportes de sync). */
export const resumirDiff = (diff: DiffLocal): string =>
  `nuevos=${diff.nuevos.length} modificados=${diff.modificados.length} ` +
  `borrados=${diff.borrados.length} sinCambios=${diff.sinCambios} yaEncolados=${diff.yaEncolados}`;

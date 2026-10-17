/**
 * SyncFilter — qué NO es contenido sincronizable (plan fase 3, paso 5; versión del
 * corte del motor).
 *
 * Es POLÍTICA ÚNICA del núcleo: el CLI y el plugin excluyen exactamente lo mismo, y el
 * escaneo la aplica en un solo lugar (`escanearVault`). Dos reglas de oro:
 *
 *  - **Excluir NO significa borrar el remoto**: si un path excluido ya estuviera en la
 *    base, el diff no propone su `del`. La exclusión empieza "hacia adelante".
 *  - La lista BLANCA selectiva de configuración portable (`.obsidian` con appearance,
 *    snippets, themes, plugins/ * /manifest|data) NO vive acá todavía: llega en el corte
 *    de filtros, porque primero hay que decidir qué hacer con `data.json` de plugins de
 *    terceros (puede contener credenciales de otros servicios) — no se sincroniza "porque sí".
 */

const PREFIJO_TEMPORAL = '.ovaultd-tmp-';

/** Carpetas vetadas por segmento exacto (en cualquier nivel del vault). */
const CARPETAS_VETADAS: ReadonlyMap<string, string> = new Map([
  ['.obsidian', 'configuración del vault (excluida entera hasta la lista blanca)'],
  ['.trash', 'papelera local del vault'],
  ['.git', 'repositorio git anidado'],
  ['node_modules', 'dependencias de node'],
]);

/** ¿Por qué este path NO se sincroniza? `null` = es contenido. */
export function motivoDeExclusion(path: string): string | null {
  for (const segmento of path.split('/')) {
    if (segmento.startsWith(PREFIJO_TEMPORAL)) {
      return 'temporal del motor';
    }

    const motivo = CARPETAS_VETADAS.get(segmento);
    if (motivo !== undefined) return motivo;
  }

  const nombre = path.slice(path.lastIndexOf('/') + 1);
  if (/^workspace(-mobile)?\.json$/.test(nombre)) {
    return 'workspace por dispositivo';
  }

  return null;
}

/** Atajo booleano para los llamadores que no muestran el motivo. */
export const estaExcluido = (path: string): boolean => motivoDeExclusion(path) !== null;

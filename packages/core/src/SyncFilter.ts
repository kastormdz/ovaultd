/**
 * SyncFilter — qué NO es contenido sincronizable (plan fase 3, paso 5; versión del
 * corte del motor, con la lista blanca de configuración portable).
 *
 * Es POLÍTICA ÚNICA del núcleo: el CLI y el plugin excluyen exactamente lo mismo, y el
 * escaneo la aplica en un solo lugar (`escanearVault`). Tres reglas de oro:
 *
 *  - **Excluir NO significa borrar el remoto**: si un path excluido ya estuviera en la
 *    base, el diff no propone su `del`. La exclusión empieza "hacia adelante".
 *  - **De `configDir` sólo se sincroniza la lista blanca de abajo.** El resto de la
 *    configuración (los `data.json` de los plugins, los workspaces, los logs) se queda
 *    local: esto viaja a TODOS los dispositivos y queda en un servidor ajeno a los
 *    servicios que esas credenciales usan. No se sincroniza "porque sí".
 *  - **Los `workspace*.json` son por dispositivo**: sincronizarlos desordena los paneles
 *    del otro lado.
 */

const PREFIJO_TEMPORAL = '.ovaultd-tmp-';

/**
 * El id del plugin de sync (permanente: de él cuelgan sus archivos locales). El filtro lo
 * conoce para excluir SU manifest: la identidad del plugin es local, no config portable.
 */
const ID_DEL_PLUGIN_DE_SYNC = 'ovaultd';

/** Carpetas vetadas por segmento exacto (en cualquier nivel del vault). */
const CARPETAS_VETADAS: ReadonlyMap<string, string> = new Map([
  ['.trash', 'papelera local del vault'],
  ['.git', 'repositorio git anidado'],
  ['node_modules', 'dependencias de node'],
]);

/**
 * La lista BLANCA de configuración portable: lo único que sale de `configDir`.
 *
 * Deliberadamente NO está:
 *  - el `data.json` de NINGÚN plugin (ni el nuestro): puede tener tokens de otros servicios;
 *  - `estado-local.json`, `sesion-cuenta.json` ni `motor.log`: son credenciales y estado del
 *    propio ovaultd, que se rearman solos con el login;
 *  - `community-plugins.json`: la lista de habilitados depende de qué plugins están
 *    INSTALADOS en cada dispositivo (el teléfono no tiene los mismos que la desktop), así
 *    que sincronizarla solo traía conflictos de bajo valor — cada toggle la reescribe;
 *  - el `manifest.json` del PROPIO ovaultd: su versión la actualiza la tienda en cada
 *    dispositivo (el binario no viaja), y el manifest sincronizado "mentía" la versión;
 *  - `workspace*.json`: son por dispositivo.
 *
 * Cada entrada es un path RELATIVO dentro de `configDir`.
 */
const CONFIG_PORTABLE: readonly RegExp[] = [
  /^appearance(\.json)?$/, // tema, tamaño de letra, acentos visuales
  /^core-plugins\.json$/, // qué plugins del núcleo están habilitados
  /^hotkeys\.json$/, // atajos de teclado (portables: no llevan rutas absolutas)
  /^snippets\/.+/, // CSS propio del vault
  /^themes\/.+/, // temas instalados como archivos
  /^plugins\/[^/]+\/manifest\.json$/, // la identidad de cada plugin, NO su data.json
];

/** ¿Este path relativo dentro de `configDir` es configuración portable? */
export function esConfigPortable(dentroDelConfig: string): boolean {
  return CONFIG_PORTABLE.some((patron) => patron.test(dentroDelConfig));
}

/**
 * ¿`dentro` es un CONTENEDOR del configDir (una carpeta que sólo cuelga configuración)?
 *
 * Excluir una carpeta la saca del escaneo ENTERA (no se recursa), así que las carpetas que
 * existen para colgar contenido portable no pueden excluirse como si fueran archivos:
 *  - `''` — la raíz del configDir (`esConfigPortable('')` no matchea nada y excluía TODO).
 *  - `snippets` / `themes` / `plugins` — las carpetas de la lista blanca.
 *  - `plugins/<id>` — el directorio de cada plugin (su `manifest.json` es portable).
 *
 * Un `plugins/<id>/data.json` NO es contenedor: es un archivo vetado y se excluye.
 */
function esContenedorDeConfig(dentro: string): boolean {
  if (dentro === '') return true;
  if (dentro === 'snippets' || dentro === 'themes' || dentro === 'plugins') return true;
  const partes = dentro.split('/');
  return partes.length === 2 && partes[0] === 'plugins';
}

/**
 * ¿Por qué este path NO se sincroniza? `null` = es contenido.
 *
 * `configDir` es el del vault (no siempre `.obsidian`): el usuario puede cambiarlo.
 */
export function motivoDeExclusion(path: string, configDir = '.obsidian'): string | null {
  const segmentos = path.split('/');

  for (const segmento of segmentos) {
    if (segmento.startsWith(PREFIJO_TEMPORAL)) {
      return 'temporal del motor';
    }

    const motivo = CARPETAS_VETADAS.get(segmento);
    if (motivo !== undefined) return motivo;
  }

  const nombre = segmentos[segmentos.length - 1];
  if (/^workspace(-mobile)?\.json$/.test(nombre)) {
    return 'workspace por dispositivo';
  }

  const i = segmentos.indexOf(configDir);
  if (i !== -1) {
    const dentro = segmentos.slice(i + 1).join('/');
    // Los contenedores se recursan (sus archivos se deciden uno a uno): la raíz del
    // configDir y las carpetas de la lista blanca NO pueden excluirse o el contenido
    // portable queda inalcanzable (el bug que dejó de sincronizar los themes).
    if (esContenedorDeConfig(dentro)) return null;
    // El manifest del PROPIO plugin de sync es local por diseño: lo actualiza la tienda en
    // cada dispositivo (el binario no viaja) y sincronizarlo "mentía" la versión.
    if (dentro === `plugins/${ID_DEL_PLUGIN_DE_SYNC}/manifest.json`) {
      return 'manifest del plugin de sync (lo actualiza la tienda en cada dispositivo)';
    }
    return esConfigPortable(dentro)
      ? null
      : 'configuración del vault (fuera de la lista blanca)';
  }

  return null;
}

/** Atajo booleano para los llamadores que no muestran el motivo. */
export const estaExcluido = (path: string, configDir = '.obsidian'): boolean =>
  motivoDeExclusion(path, configDir) !== null;

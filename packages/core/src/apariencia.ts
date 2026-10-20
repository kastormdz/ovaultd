/**
 * `appearance.json` es config COMPARTIDA solo a medias.
 *
 * - **Viaja**: el tema elegido (`cssTheme`) y los snippets habilitados (`enabledCssSnippets`)
 *   son decisiones del usuario y tienen sentido en todos los dispositivos.
 * - **NO viaja**: el modo claro/oscuro (`theme`), las fuentes y el tamaño — Obsidian los
 *   reescribe por dispositivo (el modo automático cambia al anochecer). Sincronizar el
 *   archivo entero generó 25 revisiones del mismo path entre 3 devices: colisión infinita.
 *
 * Dos piezas acá:
 *  1. `proyeccionDeApariencia` — lo que cuenta como "cambio" (hash proyectado). Un cambio
 *     de modo no genera revisión.
 *  2. `mergeDeApariencia` — merge POR CAMPO en el apply. No es el 3-vías de texto: con
 *     JSON por líneas cualquier reformateo es conflicto.
 */

import { cryptoProvider } from '@ovaultd/crypto';
import { toHex } from '@ovaultd/protocol';

/** Los únicos campos que viajan entre dispositivos. */
const CAMPOS_PORTABLES = ['cssTheme', 'enabledCssSnippets'] as const;

const DECODIFICADOR = new TextDecoder();
const CODIFICADOR = new TextEncoder();

type Json = Record<string, unknown>;

/** ¿El path es un `appearance.json` (del configDir que sea)? */
export const esApariencia = (path: string): boolean => /(^|\/)appearance\.json$/.test(path);

const parsear = (texto: string): Json | null => {
  try {
    const v = JSON.parse(texto) as unknown;
    return v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Json) : null;
  } catch {
    return null;
  }
};

/**
 * Proyección canónica: SOLO los campos portables, con los snippets ordenados. Es lo que
 * se hashea para decidir si el archivo cambió.
 */
export function proyeccionDeApariencia(texto: string): string {
  const o = parsear(texto) ?? {};
  const p: Json = {};
  for (const campo of CAMPOS_PORTABLES) {
    const v = o[campo];
    if (campo === 'enabledCssSnippets' && Array.isArray(v)) {
      p[campo] = [...v].map(String).sort();
    } else if (v !== undefined) {
      p[campo] = v;
    }
  }
  return JSON.stringify(p);
}

/**
 * Hash de un archivo para el diff/apply. Para `appearance.json` es el hash de la
 * PROYECCIÓN: así el modo que Obsidian reescribe solo no se ve como una edición.
 * Para todo lo demás, el hash de los bytes.
 */
export function hashDePath(path: string, bytes: Uint8Array): string {
  const datos = esApariencia(path)
    ? CODIFICADOR.encode(proyeccionDeApariencia(DECODIFICADOR.decode(bytes)))
    : bytes;
  return toHex(cryptoProvider().sha256(datos));
}

/**
 * Merge por campo: `{...local, ...portablesDelRemoto}`. Un campo portable solo se toma
 * del remoto si el remoto lo CAMBIÓ respecto de la base — así un cambio local del tema no
 * se pisa con el remoto viejo. El resto del archivo (modo, fuentes, claves que no
 * conocemos) queda como estaba en el local.
 *
 * JSON inválido en cualquier lado: gana el local (nunca se rompe el archivo).
 */
export function mergeDeApariencia(base: string, local: string, remoto: string): string {
  const l = parsear(local);
  const b = parsear(base);
  const r = parsear(remoto);
  if (l === null || b === null || r === null) return local;

  const salida: Json = { ...l };
  for (const campo of CAMPOS_PORTABLES) {
    if (JSON.stringify(r[campo]) !== JSON.stringify(b[campo])) {
      salida[campo] = r[campo];
    }
  }
  return `${JSON.stringify(salida, null, 2)}\n`;
}

/**
 * Validación de rutas de vault (plan de fase 3, paso 2): una ruta DESCIFRADA que llegó
 * por el wire se valida ANTES de tocar el filesystem. Vive en el core porque el peligro
 * es el mismo en todas partes: Linux acepta casi cualquier nombre, pero un vault viaja
 * entre Windows/macOS/móvil — un nombre que allá no se puede escribir rompe el sync en
 * silencio. Nada de skips silenciosos: el llamador bloquea con el motivo a la vista.
 *
 * Criterio (espejo de lo que Obsidian acepta al crear notas + el mínimo portable):
 *  - nada de absolutas (`/…`, `C:\…`) ni travesía (`.`, `..`);
 *  - sin segmentos vacíos (`a//b`, barra final);
 *  - caracteres vetados en cualquier plataforma: `< > : " | ? * \` y controles;
 *  - sin punto ni espacio final (Windows los recorta);
 *  - nombres reservados de Windows (NUL, CON, COM1…, con o sin extensión);
 *  - segmento ≤ 255 bytes UTF-8 y ruta total ≤ 1000 caracteres.
 */

export type ValidacionDeRuta = { ok: true } | { ok: false; motivo: string };

const CARACTERES_VETADOS = /[<>:"|?*\\\u0000-\u001f]/;
const RESERVADOS = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?$/i;
const MAX_SEGMENTO_BYTES = 255;
const MAX_RUTA_CHARS = 1000;

const codificador = new TextEncoder();

/** Valida una ruta relativa de vault. `''` es válido: es el tombstone sin path. */
export function validarRutaDeVault(path: string): ValidacionDeRuta {
  if (typeof path !== 'string') return { ok: false, motivo: 'no es texto' };
  if (path === '') return { ok: true };
  if (path.length > MAX_RUTA_CHARS) return { ok: false, motivo: 'ruta demasiado larga' };
  if (path.startsWith('/')) return { ok: false, motivo: 'ruta absoluta (empieza con /)' };
  if (/^[A-Za-z]:/.test(path)) return { ok: false, motivo: 'ruta absoluta (letra de unidad)' };

  const segmentos = path.split('/');
  for (const segmento of segmentos) {
    if (segmento === '') return { ok: false, motivo: 'segmento vacío (barra duplicada o final)' };
    if (segmento === '.' || segmento === '..') return { ok: false, motivo: 'travesía en la ruta' };
    if (CARACTERES_VETADOS.test(segmento)) {
      return { ok: false, motivo: `carácter vetado en «${segmento}»` };
    }
    if (segmento.endsWith('.') || segmento.endsWith(' ')) {
      return { ok: false, motivo: `«${segmento}» termina en punto o espacio` };
    }
    if (RESERVADOS.test(segmento)) {
      return { ok: false, motivo: `nombre reservado del sistema: «${segmento}»` };
    }
    if (codificador.encode(segmento).length > MAX_SEGMENTO_BYTES) {
      return { ok: false, motivo: `segmento de más de ${MAX_SEGMENTO_BYTES} bytes` };
    }
  }

  return { ok: true };
}

/** Último segmento de una ruta de vault (la ruta NO lleva barra final: ya se validó). */
export function nombreBase(path: string): string {
  const corte = path.lastIndexOf('/');
  return corte < 0 ? path : path.slice(corte + 1);
}

/**
 * ¿Algún nombre del directorio choca con `objetivo` solo por mayúsculas/normalización?
 * En filesystems case-insensitive (macOS, Windows) dos archivos así no pueden convivir;
 * detectarlo ANTES de escribir evita ese limbo. Devuelve el nombre que choca, o null.
 */
export function colisionDeCase(nombres: string[], objetivo: string): string | null {
  const plegado = (s: string): string => s.normalize('NFC').toLowerCase();
  const buscado = plegado(objetivo);

  for (const nombre of nombres) {
    if (nombre !== objetivo && plegado(nombre) === buscado) return nombre;
  }

  return null;
}

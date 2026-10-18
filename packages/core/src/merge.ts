/**
 * merge.ts — merge de texto a 3 vías (base, ours, theirs), char-level (plan fase 3, paso 4).
 *
 * La composición es la del protocolo, y no es "usar diff-match-patch" a secas:
 *
 *  1. `d_ours = diff(base, ours)` y `d_theirs = diff(base, theirs)`, con los diffs CRUDOS.
 *  2. Los dos se PROYECTAN sobre la base: si sus rangos se solapan, es **conflicto**.
 *  3. Sin solapamiento, la fusión se ARMA recorriendo la base: determinística, sin fuzzy.
 *
 * Por qué no se usan `patch_make`/`patch_apply` para decidir ni para fusionar: los parches
 * llevan limpieza semántica, que los agranda para que "se lean mejor". Con eso, dos ediciones
 * disjuntas de un texto corto parecen solapadas y un parche puede terminar abarcando el texto
 * entero. Es exactamente el fuzzy contra el que advierte el plan: acá no decide nada.
 *
 * Reglas que salen de ahí:
 *  - El fuzzy matching jamás decide por su cuenta: un lugar ambiguo es conflicto.
 *  - Dos inserciones concurrentes en el mismo punto son conflicto (no se inventa un orden).
 *  - Dos ediciones CONTIGUAS (una termina donde empieza la otra) NO se pisan: se fusionan.
 *  - Quien llama decide QUÉ hacer con un conflicto (`protocol.md`: markers visibles para una
 *    revisión, o `nombre (conflicted copy <fecha>).md`). Acá sólo se decide si se puede fusionar.
 *  - Binarios, `.canvas` y `.base` NO se fusionan nunca: eso lo decide el llamador por tipo.
 */

import DiffMatchPatch from 'diff-match-patch';

export type ResultadoMerge = { tipo: 'merge'; texto: string } | { tipo: 'conflicto' };

/** Un cambio en coordenadas de la BASE: se reemplaza `[inicio, fin)` por `texto`. */
interface Cambio {
  inicio: number;
  fin: number;
  texto: string;
}

/**
 * Los cambios reales de `base` a `texto`, con su reemplazo, en coordenadas de la base.
 *
 * Un borrado seguido (o precedido) por una inserción es UN cambio (`b` -> `b MOD`), no dos.
 * Una inserción sola es `[p, p)`.
 */
function cambiosDe(dmp: DiffMatchPatch, base: string, texto: string): Cambio[] {
  const cambios: Cambio[] = [];
  let pendiente: Cambio | null = null;
  let pos = 0;

  const cerrar = (): void => {
    if (pendiente !== null) {
      cambios.push(pendiente);
      pendiente = null;
    }
  };

  for (const [op, chunk] of dmp.diff_main(base, texto)) {
    if (op === 0) {
      cerrar();
      pos += chunk.length;
      continue;
    }

    if (op === -1) {
      if (pendiente === null) {
        pendiente = { inicio: pos, fin: pos + chunk.length, texto: '' };
      } else {
        pendiente.fin = pos + chunk.length;
      }
      pos += chunk.length;
      continue;
    }

    // Inserción: se acumula en el cambio en curso o abre uno de ancho cero.
    if (pendiente === null) {
      pendiente = { inicio: pos, fin: pos, texto: chunk };
    } else {
      pendiente.texto += chunk;
    }
  }

  cerrar();
  return cambios;
}

/** ¿Algún cambio de `a` se pisa con alguno de `b`? */
function seSolapan(a: readonly Cambio[], b: readonly Cambio[]): boolean {
  for (const p of a) {
    for (const q of b) {
      const pEsPunto = p.inicio === p.fin;
      const qEsPunto = q.inicio === q.fin;

      if (pEsPunto && qEsPunto) {
        // Dos inserciones concurrentes en el MISMO punto: conflicto, no un orden arbitrario.
        if (p.inicio === q.inicio) return true;
        continue;
      }
      if (pEsPunto) {
        if (p.inicio >= q.inicio && p.inicio < q.fin) return true;
        continue;
      }
      if (qEsPunto) {
        if (q.inicio >= p.inicio && q.inicio < p.fin) return true;
        continue;
      }
      // Intervalos [inicio, fin): dos ediciones CONTIGUAS no se pisan.
      if (p.inicio < q.fin && q.inicio < p.fin) return true;
    }
  }
  return false;
}

/**
 * Fusiona `ours` y `theirs` sobre `base`. `conflicto` significa "esto lo decide el usuario":
 * ninguna parte del merge se aplica a medias.
 */
export function mergeDeTexto(base: string, ours: string, theirs: string): ResultadoMerge {
  // Atajos: si uno no cambió, gana el otro (el caso más común, y sin diffear).
  if (base === ours) return { tipo: 'merge', texto: theirs };
  if (base === theirs) return { tipo: 'merge', texto: ours };
  if (ours === theirs) return { tipo: 'merge', texto: ours };

  const dmp = new DiffMatchPatch();

  const cambiosOurs = cambiosDe(dmp, base, ours);
  const cambiosTheirs = cambiosDe(dmp, base, theirs);

  if (cambiosOurs.length === 0) return { tipo: 'merge', texto: theirs };
  if (cambiosTheirs.length === 0) return { tipo: 'merge', texto: ours };

  if (seSolapan(cambiosOurs, cambiosTheirs)) return { tipo: 'conflicto' };

  // Disjuntos: se recorren los cambios de los dos lados sobre la base y se arma la fusión.
  // Determinístico — no hay lugar donde el fuzzy pueda "elegir" ni dejar un resultado parcial.
  const todos = [...cambiosOurs, ...cambiosTheirs].sort((a, b) => a.inicio - b.inicio);
  let fusionado = '';
  let cursor = 0;
  for (const cambio of todos) {
    fusionado += base.slice(cursor, cambio.inicio) + cambio.texto;
    cursor = cambio.fin;
  }
  fusionado += base.slice(cursor);

  return { tipo: 'merge', texto: fusionado };
}

/** ¿Este path admite merge de texto? Binarios, canvas y bases: nunca. */
export function esMergeablePorTipo(path: string): boolean {
  const nombre = path.slice(path.lastIndexOf('/') + 1).toLowerCase();
  if (/\.(canvas|base)$/.test(nombre)) return false;
  // texto plano conocido: markdown y lo que Obsidian trata como texto
  return /\.(md|markdown|txt|json|css|js|ts|yml|yaml|csv)$/.test(nombre);
}

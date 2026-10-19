/**
 * Corpus de merge 3-vías (fase 5, paso 4 del plan): `tests/fixtures/merge/cases.json`
 * ejercita `mergeDeTexto` con los patrones de documento que aparecen en un vault real
 * (frontmatter YAML, tablas, listas, code fences, Unicode, párrafos largos, wiki-links).
 *
 * La regla de oro del fallback: cuando el resultado NO es seguro (`esperado: null`), el
 * merge DEVUELVE conflicto y el motor conserva ambas versiones — el fuzzy jamás decide.
 */

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import { mergeDeTexto } from '../src/merge.js';

interface CasoDelCorpus {
  nombre: string;
  base: string;
  nuestro: string;
  suyo: string;
  esperado: string | null;
  nota: string;
}

const rutaCorpus = fileURLToPath(
  new URL('../../../tests/fixtures/merge/cases.json', import.meta.url),
);
const corpus = JSON.parse(readFileSync(rutaCorpus, 'utf8')) as { casos: CasoDelCorpus[] };

describe('corpus de merge 3-vías (fixtures/merge/cases.json)', () => {
  it('el corpus cubre los patrones del plan y tiene conflictos de verdad', () => {
    expect(corpus.casos.length).toBeGreaterThanOrEqual(14);
    const nombres = corpus.casos.map((caso) => caso.nombre).join('\n');
    for (const patron of [
      'frontmatter',
      'tabla',
      'lista',
      'code fence',
      'Unicode',
      'parrafos largos',
    ]) {
      expect(nombres).toContain(patron);
    }
    // Sin casos de conflicto, el corpus probaría la mitad de la historia.
    expect(corpus.casos.filter((caso) => caso.esperado === null).length).toBeGreaterThanOrEqual(5);
  });

  for (const caso of corpus.casos) {
    it(`${caso.nombre} — ${caso.nota}`, () => {
      const resultado = mergeDeTexto(caso.base, caso.nuestro, caso.suyo);
      if (caso.esperado === null) {
        expect(resultado.tipo).toBe('conflicto');
        return;
      }
      expect(resultado).toEqual({ tipo: 'merge', texto: caso.esperado });
    });
  }
});

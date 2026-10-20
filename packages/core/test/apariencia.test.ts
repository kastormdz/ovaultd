/**
 * `appearance.json`: config compartida a medias. El tema y los snippets viajan; el modo
 * claro/oscuro y las fuentes los reescribe Obsidian por dispositivo y nunca se aplican
 * del remoto. Estos tests fijan la proyección (lo que viaja) y el merge por campo.
 */

import { describe, expect, it } from 'vitest';

import { esApariencia, mergeDeApariencia, proyeccionDeApariencia } from '../src/apariencia.js';

const j = (o: object): string => `${JSON.stringify(o, null, 2)}\n`;

describe('apariencia', () => {
  it('detecta el archivo por nombre', () => {
    expect(esApariencia('.obsidian/appearance.json')).toBe(true);
    expect(esApariencia('appearance.json')).toBe(true);
    expect(esApariencia('.obsidian/community-plugins.json')).toBe(false);
    expect(esApariencia('.obsidian/themes/Anu/theme.css')).toBe(false);
  });

  it('la proyección solo mira los campos portables y ordena los snippets', () => {
    const a = proyeccionDeApariencia(
      j({ cssTheme: 'Anu', theme: 'obsidian', enabledCssSnippets: ['b', 'a'] }),
    );
    const b = proyeccionDeApariencia(
      j({ cssTheme: 'Anu', theme: 'moonstone', enabledCssSnippets: ['a', 'b'] }),
    );
    expect(a).toBe(b); // el modo y el orden no cuentan
    expect(proyeccionDeApariencia(j({ cssTheme: 'Otro', enabledCssSnippets: [] }))).not.toBe(a);
  });

  it('el remoto gana en el tema y el local conserva su modo y sus fuentes', () => {
    const base = j({ cssTheme: 'Anu', theme: 'obsidian', baseFontSize: 16 });
    const local = j({ cssTheme: 'Anu', theme: 'moonstone', baseFontSize: 18 });
    const remoto = j({ cssTheme: 'Blue Topaz', theme: 'obsidian', baseFontSize: 16 });

    const r = JSON.parse(mergeDeApariencia(base, local, remoto)) as Record<string, unknown>;

    expect(r.cssTheme).toBe('Blue Topaz'); // viaja
    expect(r.theme).toBe('moonstone'); // local
    expect(r.baseFontSize).toBe(18); // local
  });

  it('un cambio SOLO local del tema no se pisa con el remoto viejo', () => {
    const base = j({ cssTheme: 'Anu' });
    const local = j({ cssTheme: 'Cosmic' });
    const remoto = j({ cssTheme: 'Anu' });

    const r = JSON.parse(mergeDeApariencia(base, local, remoto)) as Record<string, unknown>;
    expect(r.cssTheme).toBe('Cosmic');
  });

  it('conserva claves desconocidas del device (no las borra el remoto)', () => {
    const base = j({ cssTheme: 'Anu' });
    const local = j({ cssTheme: 'Anu', campoNuevoDeObsidian: { x: 1 } });
    const remoto = j({ cssTheme: 'Cosmic' });

    const r = JSON.parse(mergeDeApariencia(base, local, remoto)) as Record<string, unknown>;
    expect(r.cssTheme).toBe('Cosmic');
    expect(r.campoNuevoDeObsidian).toEqual({ x: 1 });
  });

  it('JSON inválido en cualquiera de los tres lados: gana el local', () => {
    const local = j({ cssTheme: 'Cosmic' });
    expect(mergeDeApariencia('{roto', local, j({ cssTheme: 'Anu' }))).toBe(local);
    expect(mergeDeApariencia(j({ cssTheme: 'Anu' }), '{roto', j({ cssTheme: 'Anu' }))).toBe('{roto');
  });
});

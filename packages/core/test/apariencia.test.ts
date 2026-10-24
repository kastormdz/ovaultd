import { createHash } from 'node:crypto';
/**
 * `appearance.json`: config compartida a medias. El tema y los snippets viajan; el modo
 * claro/oscuro y las fuentes los reescribe Obsidian por dispositivo y nunca se aplican
 * del remoto. Estos tests fijan la proyección (lo que viaja) y el merge por campo.
 */

import { describe, expect, it } from 'vitest';

import {
  bytesParaHash,
  esApariencia,
  mergeDeApariencia,
  proyeccionDeApariencia,
} from '../src/apariencia.js';

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

describe('bytesParaHash (A2: el hash de apariencia sale de la proyección)', () => {
  const enc = new TextEncoder();
  const APARIENCIA = '.obsidian/appearance.json';
  const hash = (path: string, texto: string): string =>
    createHash('sha256').update(bytesParaHash(path, enc.encode(texto))).digest('hex');

  it('un cambio de MODO no cambia el hash (no genera revisión)', () => {
    const oscuro = '{"theme":"obsidian","cssTheme":"Minimal","enabledCssSnippets":["a"]}';
    const claro = '{"theme":"moonstone","cssTheme":"Minimal","enabledCssSnippets":["a"]}';
    expect(hash(APARIENCIA, oscuro)).toBe(hash(APARIENCIA, claro));
  });

  it('un cambio de TEMA o de snippets SÍ cambia el hash', () => {
    const base = '{"theme":"obsidian","cssTheme":"Minimal","enabledCssSnippets":["a"]}';
    expect(hash(APARIENCIA, base)).not.toBe(
      hash(APARIENCIA, '{"theme":"obsidian","cssTheme":"Blue Topaz","enabledCssSnippets":["a"]}'),
    );
    expect(hash(APARIENCIA, base)).not.toBe(
      hash(APARIENCIA, '{"theme":"obsidian","cssTheme":"Minimal","enabledCssSnippets":["a","b"]}'),
    );
  });

  it('el orden de los snippets no cuenta como cambio', () => {
    const uno = '{"cssTheme":"Minimal","enabledCssSnippets":["a","b"]}';
    const otro = '{"cssTheme":"Minimal","enabledCssSnippets":["b","a"]}';
    expect(hash(APARIENCIA, uno)).toBe(hash(APARIENCIA, otro));
  });

  it('cualquier otro archivo se hashea crudo (no se toca)', () => {
    const texto = '{"theme":"obsidian"}';
    expect(hash('notas/x.md', texto)).toBe(
      createHash('sha256').update(enc.encode(texto)).digest('hex'),
    );
  });
});

describe('mergeDeApariencia: corpus por campo (F-08)', () => {
  const APARIENCIA = '.obsidian/appearance.json';
  const casos: Array<{
    caso: string;
    base: string;
    local: string;
    remoto: string;
    esperado: Record<string, unknown>;
  }> = [
    {
      caso: 'el remoto cambió el tema: se toma',
      base: '{"cssTheme":"Minimal","theme":"obsidian"}',
      local: '{"cssTheme":"Minimal","theme":"obsidian"}',
      remoto: '{"cssTheme":"Blue Topaz","theme":"obsidian"}',
      esperado: { cssTheme: 'Blue Topaz', theme: 'obsidian' },
    },
    {
      caso: 'el remoto NO cambió el tema: el local manda',
      base: '{"cssTheme":"Minimal"}',
      local: '{"cssTheme":"Otro"}',
      remoto: '{"cssTheme":"Minimal"}',
      esperado: { cssTheme: 'Otro' },
    },
    {
      caso: 'el modo (no portable) NO viaja aunque cambie en el remoto',
      base: '{"theme":"obsidian"}',
      local: '{"theme":"moonstone"}',
      remoto: '{"theme":"obsidian"}',
      esperado: { theme: 'moonstone' },
    },
    {
      caso: 'las claves desconocidas del local se conservan',
      base: '{}',
      local: '{"miClaveRara":123,"cssTheme":"Minimal"}',
      remoto: '{"cssTheme":"Blue Topaz"}',
      esperado: { miClaveRara: 123, cssTheme: 'Blue Topaz' },
    },
    {
      caso: 'los snippets del remoto reemplazan a los del local',
      base: '{"enabledCssSnippets":["a"]}',
      local: '{"enabledCssSnippets":["a"]}',
      remoto: '{"enabledCssSnippets":["a","b"]}',
      esperado: { enabledCssSnippets: ['a', 'b'] },
    },
    {
      caso: 'un cambio de fuentes en el remoto no toca nada',
      base: '{"cssTheme":"Minimal"}',
      local: '{"cssTheme":"Minimal","textFontFamily":"Inter"}',
      remoto: '{"cssTheme":"Minimal","textFontFamily":"Otra"}',
      esperado: { cssTheme: 'Minimal', textFontFamily: 'Inter' },
    },
    {
      caso: 'los dos cambiaron el tema: gana el remoto (es el que llega)',
      base: '{"cssTheme":"Minimal"}',
      local: '{"cssTheme":"Local"}',
      remoto: '{"cssTheme":"Remoto"}',
      esperado: { cssTheme: 'Remoto' },
    },
  ];

  for (const { caso, base, local, remoto, esperado } of casos) {
    it(caso, () => {
      const salida = JSON.parse(mergeDeApariencia(base, local, remoto)) as Record<string, unknown>;
      expect(salida).toEqual(esperado);
    });
  }

  it('JSON inválido en el local: gana el local tal cual (nunca se rompe el archivo)', () => {
    expect(mergeDeApariencia('{}', 'no es json', '{"cssTheme":"X"}')).toBe('no es json');
  });

  it('JSON inválido en el remoto: gana el local', () => {
    const local = '{"cssTheme":"Minimal"}';
    expect(mergeDeApariencia('{}', local, 'roto')).toBe(local);
  });

  it('la salida es JSON con 2 espacios y salto final (no reformatea el vault)', () => {
    const salida = mergeDeApariencia('{}', '{"a":1}', '{"a":1}');
    expect(salida.endsWith('\n')).toBe(true);
    expect(salida).toContain('\n  "a": 1');
  });
});

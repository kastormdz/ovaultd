import { describe, expect, it } from 'vitest';

import { colisionDeCase, nombreBase, validarRutaDeVault } from '../src/vault-path.js';

describe('validarRutaDeVault', () => {
  const aceptadas = [
    '',
    'Notas/uno.md',
    'a/b/c/d.md',
    'con-espacios y acentos/ñandú.md',
    'punto.normal.md',
    'a/B/C.PDF',
    '2024-01-01 diario.md',
  ];

  const rechazadas: Array<[string, string]> = [
    ['/etc/passwd', 'absoluta'],
    ['C:\\Users\\x', 'unidad'],
    ['../fuera.md', 'travesía'],
    ['a/../b.md', 'travesía'],
    ['a/./b.md', 'travesía'],
    ['a//b.md', 'vacío'],
    ['carpeta/', 'vacío'],
    ['.', 'travesía'],
    ['a/nul', 'reservado'],
    ['CON.md', 'reservado'],
    ['aux.txt', 'reservado'],
    ['com1.md', 'reservado'],
    ['a<raro>.md', 'vetado'],
    ['di:ario.md', 'vetado'],
    ['pre|gunta.md', 'vetado'],
    ['estrella*.md', 'vetado'],
    ['interrogacion?.md', 'vetado'],
    ['barra\\inversa.md', 'vetado'],
    ['fin con espacio .md ', 'espacio'],
    ['fin con punto.md.', 'punto'],
    ['control\u0007.md', 'vetado'],
  ];

  for (const ruta of aceptadas) {
    it(`acepta ${JSON.stringify(ruta)}`, () => {
      expect(validarRutaDeVault(ruta)).toEqual({ ok: true });
    });
  }

  for (const [ruta, aguja] of rechazadas) {
    it(`rechaza ${JSON.stringify(ruta)} (${aguja})`, () => {
      const resultado = validarRutaDeVault(ruta);
      expect(resultado.ok).toBe(false);
      if (!resultado.ok) expect(resultado.motivo).toContain(aguja);
    });
  }

  it('rechaza segmentos de más de 255 bytes UTF-8', () => {
    const largo = 'ñ'.repeat(130); // 260 bytes
    const resultado = validarRutaDeVault(`dir/${largo}.md`);
    expect(resultado.ok).toBe(false);
    if (!resultado.ok) expect(resultado.motivo).toContain('255');
  });
});

describe('nombreBase', () => {
  it('devuelve el último segmento', () => {
    expect(nombreBase('a/b/c.md')).toBe('c.md');
    expect(nombreBase('solo.md')).toBe('solo.md');
  });
});

describe('colisionDeCase', () => {
  it('detecta choque solo por mayúsculas', () => {
    expect(colisionDeCase(['Nota.md', 'otra.md'], 'nota.md')).toBe('Nota.md');
  });

  it('detecta choque por normalización Unicode (NFC/NFD)', () => {
    expect(colisionDeCase(['ñandú.md'.normalize('NFD')], 'ñandú.md'.normalize('NFC'))).toBe(
      'ñandú.md'.normalize('NFD'),
    );
  });

  it('no marca el mismo nombre exacto', () => {
    expect(colisionDeCase(['Nota.md'], 'Nota.md')).toBeNull();
  });

  it('no marca nombres distintos', () => {
    expect(colisionDeCase(['Nota.md', 'apunte.md'], 'otra.md')).toBeNull();
  });
});

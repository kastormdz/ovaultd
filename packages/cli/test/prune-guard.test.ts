import { describe, expect, it } from 'vitest';

import { guardaDePrune } from '@ovaultd/core';

describe('guarda de prune-remote (F-03)', () => {
  it('aborta si el disco está vacío y el server tiene paths', () => {
    const mensaje = guardaDePrune({ vivos: 232, enDisco: 0, huerfanos: ['a.md', 'b.md'] }, false);
    expect(mensaje).toContain('borraría TODO');
    expect(mensaje).toContain('232');
  });

  it('con --permitir-vacio deja pasar el caso intencional', () => {
    expect(guardaDePrune({ vivos: 232, enDisco: 0, huerfanos: ['a.md'] }, true)).toBeNull();
  });

  it('con archivos en disco no interviene', () => {
    expect(guardaDePrune({ vivos: 232, enDisco: 232, huerfanos: [] }, false)).toBeNull();
  });

  it('con el server vacío no hay nada que proteger', () => {
    expect(guardaDePrune({ vivos: 0, enDisco: 0, huerfanos: [] }, false)).toBeNull();
  });

  it('aborta si borraría más de la mitad de los paths del server (umbral)', () => {
    const mensaje = guardaDePrune(
      { vivos: 10, enDisco: 4, huerfanos: ['a.md', 'b.md', 'c.md', 'd.md', 'e.md', 'f.md'] },
      false,
    );
    expect(mensaje).toContain('más de la mitad');
    expect(mensaje).toContain('60 %');
  });

  it('deja pasar un plan chico (menos de la mitad)', () => {
    const mensaje = guardaDePrune(
      { vivos: 10, enDisco: 8, huerfanos: ['a.md', 'b.md'] },
      false,
    );
    expect(mensaje).toBeNull();
  });
});

import { describe, expect, it } from 'vitest';

import { guardaDePrune } from '../src/prune-guard.js';

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
});

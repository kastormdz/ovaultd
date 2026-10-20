import { describe, expect, it } from 'vitest';

import { HEADER_BRIDGE, tokenValido } from '../src/browser-runtime.js';

describe('token del puente (F-02)', () => {
  const token = 'a'.repeat(64);

  it('acepta el token exacto de la sesión', () => {
    expect(tokenValido(token, token)).toBe(true);
  });

  it('rechaza el request sin token (PoC P4 de la auditoría)', () => {
    expect(tokenValido(undefined, token)).toBe(false);
    expect(tokenValido('', token)).toBe(false);
  });

  it('rechaza un token de otro largo y otro contenido', () => {
    expect(tokenValido('corto', token)).toBe(false);
    expect(tokenValido('b'.repeat(64), token)).toBe(false);
  });

  it('el header es el documentado', () => {
    expect(HEADER_BRIDGE).toBe('x-ovaultd-bridge');
  });
});

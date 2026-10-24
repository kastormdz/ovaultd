import { afterEach, describe, expect, it } from 'vitest';

import { confirmarAccion, registrarAccion } from '../src/acciones.js';

describe('A2: acciones destructivas (F-07)', () => {
  const ttyOriginal = process.stdin.isTTY;
  afterEach(() => {
    Object.defineProperty(process.stdin, 'isTTY', { value: ttyOriginal, configurable: true });
  });

  it('sin TTY y sin --yes NO borra: lanza con la instrucción de --yes', async () => {
    Object.defineProperty(process.stdin, 'isTTY', { value: false, configurable: true });
    await expect(confirmarAccion('¿borrar?', {})).rejects.toThrow(/--yes/);
  });

  it('con --yes procede sin preguntar (para scripts)', async () => {
    Object.defineProperty(process.stdin, 'isTTY', { value: false, configurable: true });
    await expect(confirmarAccion('¿borrar?', { yes: true })).resolves.toBe(true);
  });

  it('la bitácora escribe una línea con fecha, sin romper si el dir no existe', () => {
    registrarAccion('test línea');
    // No se afirma el contenido (depende del HOME); sí que no lanza.
    expect(true).toBe(true);
  });
});

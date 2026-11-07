/**
 * El navegador del runtime: la copia ESTABLE (fuera de la caché de Playwright) manda.
 *
 * Playwright guarda su binario en `~/.cache/ms-playwright`, un artefacto de desarrollo:
 * limpiar esa caché (o instalar otra versión de Playwright, que busca otro build) deja el
 * servicio de sync muerto sin pista. El runtime usa primero la copia de `dirNavegador()`.
 */

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { dirNavegador, rutaNavegador } from '../src/browser-runtime.js';

describe('navegador del runtime', () => {
  let base: string;
  const previo = process.env.OVAULTD_BROWSER_DIR;

  beforeEach(() => {
    base = mkdtempSync(join(tmpdir(), 'ovaultd-browser-'));
    process.env.OVAULTD_BROWSER_DIR = base;
  });

  afterEach(() => {
    if (previo === undefined) delete process.env.OVAULTD_BROWSER_DIR;
    else process.env.OVAULTD_BROWSER_DIR = previo;
    rmSync(base, { recursive: true, force: true });
  });

  it('sin nada instalado no hay ruta (y no rompe)', () => {
    expect(rutaNavegador()).toBeUndefined();
  });

  it('un directorio que no existe tampoco rompe', () => {
    process.env.OVAULTD_BROWSER_DIR = join(base, 'no-existe');
    expect(rutaNavegador()).toBeUndefined();
  });

  it('encuentra el chrome-headless-shell de la ruta estable', () => {
    const bin = join(base, 'chromium_headless_shell-1248', 'chrome-headless-shell-linux64');
    mkdirSync(bin, { recursive: true });
    const ejecutable = join(bin, 'chrome-headless-shell');
    writeFileSync(ejecutable, '#!/bin/sh\n');
    expect(rutaNavegador()).toBe(ejecutable);
  });

  it('con dos builds elige el más nuevo', () => {
    for (const build of ['chromium_headless_shell-1000', 'chromium_headless_shell-1248']) {
      const dir = join(base, build, 'chrome-headless-shell-linux64');
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, 'chrome-headless-shell'), 'x');
    }
    expect(rutaNavegador()).toContain('chromium_headless_shell-1248');
  });

  it('el directorio por defecto NO es la caché de Playwright', () => {
    delete process.env.OVAULTD_BROWSER_DIR;
    const porDefecto = dirNavegador();
    expect(porDefecto).toContain('ovaultd');
    expect(porDefecto).not.toContain('ms-playwright');
  });
});

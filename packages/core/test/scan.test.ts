//! El escaneo del vault COMPLETO: la recursión y las exclusiones juntas — el test que
//! faltaba cuando el sync de config no veía `.obsidian`: el filtro por path andaba bien,
//! pero la exclusión de la CARPETA raíz dejaba el contenido portable inalcanzable (el scan
//! no recursa una carpeta excluida, así que ni siquiera llegaba a consultarle los hijos).

import { deriveSubkeys, randomSalt, randomVaultKey } from '@ovaultd/crypto';
import { describe, expect, it } from 'vitest';

import type { FileSystemPort, VaultDirEntry } from '../src/file-port.js';
import { escanearVault } from '../src/scan.js';

const CLAVES = deriveSubkeys(randomVaultKey(), randomSalt());

/** Port de solo lectura que arma el árbol desde las rutas (al scan le alcanza con list/read/stat). */
function portDeArbol(rutas: string[]): FileSystemPort {
  const archivos = new Set(rutas);
  const cod = new TextEncoder();
  const contenido = (ruta: string) => cod.encode(`contenido de ${ruta}`);

  const port = {
    async list(relDir: string): Promise<VaultDirEntry[]> {
      const dir = relDir === '.' ? '' : relDir;
      const prefijo = dir === '' ? '' : `${dir}/`;
      const hijos = new Map<string, VaultDirEntry>();
      for (const ruta of archivos) {
        if (!ruta.startsWith(prefijo) || ruta === prefijo) continue;
        const resto = ruta.slice(prefijo.length);
        const corte = resto.indexOf('/');
        const nombre = corte === -1 ? resto : resto.slice(0, corte);
        const esDir = corte !== -1;
        if (!hijos.get(nombre) || esDir) {
          hijos.set(nombre, { name: nombre, isFile: !esDir, isDirectory: esDir });
        }
      }
      return [...hijos.values()];
    },
    async read(ruta: string) {
      if (!archivos.has(ruta)) throw new Error(`no existe: ${ruta}`);
      return contenido(ruta);
    },
    async stat(ruta: string) {
      if (!archivos.has(ruta)) return null;
      return { isFile: true, isDirectory: false, size: contenido(ruta).length };
    },
  };

  return port as unknown as FileSystemPort;
}

describe('escanearVault', () => {
  it('entra al configDir y devuelve SÓLO la configuración portable', async () => {
    const port = portDeArbol([
      'Notas/una.md',
      '.obsidian/appearance.json',
      '.obsidian/hotkeys.json',
      '.obsidian/workspace.json',
      '.obsidian/data.json',
      '.obsidian/themes/Mi Tema.css',
      '.obsidian/snippets/mio.css',
      '.obsidian/plugins/dataview/manifest.json',
      '.obsidian/plugins/dataview/data.json',
      '.trash/vieja.md',
    ]);

    const entradas = await escanearVault(port, CLAVES);
    const paths = entradas.map((entrada) => entrada.path).sort();

    expect(paths).toEqual([
      '.obsidian/appearance.json',
      '.obsidian/hotkeys.json',
      '.obsidian/plugins/dataview/manifest.json',
      '.obsidian/snippets/mio.css',
      '.obsidian/themes/Mi Tema.css',
      'Notas/una.md',
    ]);
    expect(paths.some((path) => path.includes('data.json'))).toBe(false);
    expect(paths.some((path) => path.includes('workspace'))).toBe(false);
    expect(paths.some((path) => path.includes('.trash'))).toBe(false);
  });

  it('un theme nuevo se escanea con su hash (el caso real del sync de config)', async () => {
    const port = portDeArbol(['.obsidian/themes/Recien Puesto.css']);
    const entradas = await escanearVault(port, CLAVES);

    expect(entradas).toHaveLength(1);
    expect(entradas[0]!.path).toBe('.obsidian/themes/Recien Puesto.css');
    expect(entradas[0]!.contentHash).toMatch(/^[0-9a-f]{64}$/);
  });
});

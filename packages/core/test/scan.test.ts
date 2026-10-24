//! El escaneo del vault COMPLETO: la recursión y las exclusiones juntas — el test que
//! faltaba cuando el sync de config no veía `.obsidian`: el filtro por path andaba bien,
//! pero la exclusión de la CARPETA raíz dejaba el contenido portable inalcanzable (el scan
//! no recursa una carpeta excluida, así que ni siquiera llegaba a consultarle los hijos).

import { deriveSubkeys, pathId as calcularPathId, randomSalt, randomVaultKey } from '@ovaultd/crypto';
import { toHex } from '@ovaultd/protocol';
import { describe, expect, it } from 'vitest';

import type { FileSystemPort, VaultDirEntry } from '../src/file-port.js';
import { diffLocal, escanearVault } from '../src/scan.js';
import type { StateStore } from '../src/StateStore.js';

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
      '.obsidian/snippets/mio.css',
      '.obsidian/themes/Mi Tema.css',
      'Notas/una.md',
    ]);
    expect(paths.some((path) => path.includes('data.json'))).toBe(false);
    expect(paths.some((path) => path.includes('workspace'))).toBe(false);
    expect(paths.some((path) => path.includes('.trash'))).toBe(false);
    // appearance.json SÍ viaja: el merge por campo se encarga del modo (apariencia.ts).
    expect(paths.some((path) => path.includes('appearance'))).toBe(true);
    expect(paths.some((path) => path.includes('hotkeys'))).toBe(false);
    expect(paths.some((path) => path.includes('manifest'))).toBe(false);
  });

  it('un theme nuevo se escanea con su hash (el caso real del sync de config)', async () => {
    const port = portDeArbol(['.obsidian/themes/Recien Puesto.css']);
    const entradas = await escanearVault(port, CLAVES);

    expect(entradas).toHaveLength(1);
    expect(entradas[0]!.path).toBe('.obsidian/themes/Recien Puesto.css');
    expect(entradas[0]!.contentHash).toMatch(/^[0-9a-f]{64}$/);
  });
});

describe('escaneo incremental (cache por mtime+size)', () => {
  it('no lee el contenido si el archivo no cambió desde el último ciclo', async () => {
    let lecturas = 0;
    const port = {
      list: async () => [{ name: 'nota.md', isFile: true, isDirectory: false }],
      read: async () => {
        lecturas += 1;
        return new TextEncoder().encode('contenido');
      },
      stat: async () => ({ size: 9, mtimeMs: 1_700_000_000_000, isFile: true, isDirectory: false }),
    } as unknown as FileSystemPort;
    const cache = new Map([
      ['nota.md', { mtime: 1_700_000_000_000, size: 9, contentHash: 'f'.repeat(64) }],
    ]);

    const entradas = await escanearVault(port, CLAVES, '', cache);

    expect(lecturas).toBe(0); // el hash salió del índice, sin tocar el disco
    expect(entradas).toHaveLength(1);
    expect(entradas[0]!.contentHash).toBe('f'.repeat(64));
    expect(entradas[0]!.mtimeMs).toBe(1_700_000_000_000);
  });

  it('vuelve a leer y hashear si el mtime o el tamaño cambiaron', async () => {
    let lecturas = 0;
    const port = {
      list: async () => [{ name: 'nota.md', isFile: true, isDirectory: false }],
      read: async () => {
        lecturas += 1;
        return new TextEncoder().encode('contenido');
      },
      stat: async () => ({ size: 10, mtimeMs: 1_700_000_000_500, isFile: true, isDirectory: false }),
    } as unknown as FileSystemPort;
    const cache = new Map([
      ['nota.md', { mtime: 1_700_000_000_000, size: 9, contentHash: 'f'.repeat(64) }],
    ]);

    const entradas = await escanearVault(port, CLAVES, '', cache);

    expect(lecturas).toBe(1); // cambió: se re-hashea
    expect(entradas[0]!.contentHash).toMatch(/^[0-9a-f]{64}$/);
    expect(entradas[0]!.contentHash).not.toBe('f'.repeat(64));
  });
});

describe('reads masivos (readMany)', () => {
  it('usa UN solo round-trip para todo el vault (sin un read por archivo)', async () => {
    let readManyLlamadas = 0;
    let readsIndividuales = 0;
    const archivos = ['a.md', 'b.md', 'c.md'];
    const port = {
      list: async () => archivos.map((name) => ({ name, isFile: true, isDirectory: false })),
      read: async (ruta: string) => {
        readsIndividuales += 1;
        return new TextEncoder().encode(`contenido ${ruta}`);
      },
      stat: async () => ({ size: 12, mtimeMs: 1_700_000_000_000, isFile: true, isDirectory: false }),
      readMany: async (rutas: string[]) => {
        readManyLlamadas += 1;
        return rutas.map((ruta) => new TextEncoder().encode(`contenido ${ruta}`));
      },
    } as unknown as FileSystemPort;

    const entradas = await escanearVault(port, CLAVES);

    expect(readManyLlamadas).toBe(1); // UN solo round-trip para los 3
    expect(readsIndividuales).toBe(0); // ni un read suelto
    expect(entradas).toHaveLength(3);
  });

  it('sin readMany en el port cae al bucle clásico de read', async () => {
    let readsIndividuales = 0;
    const port = {
      list: async () => [{ name: 'a.md', isFile: true, isDirectory: false }],
      read: async () => {
        readsIndividuales += 1;
        return new TextEncoder().encode('hola');
      },
      stat: async () => ({ size: 4, mtimeMs: 1, isFile: true, isDirectory: false }),
    } as unknown as FileSystemPort;

    const entradas = await escanearVault(port, CLAVES);

    expect(readsIndividuales).toBe(1);
    expect(entradas).toHaveLength(1);
  });
});

describe('stat masivo (statMany)', () => {
  it('usa UN solo round-trip para todo el vault (sin un stat por archivo)', async () => {
    let statManyLlamadas = 0;
    let statsIndividuales = 0;
    const archivos = ['a.md', 'b.md', 'c.md'];
    const port = {
      list: async () => archivos.map((name) => ({ name, isFile: true, isDirectory: false })),
      read: async (ruta: string) => new TextEncoder().encode(`contenido ${ruta}`),
      stat: async () => {
        statsIndividuales += 1;
        return { size: 12, mtimeMs: 1_700_000_000_000, isFile: true, isDirectory: false };
      },
      statMany: async (rutas: string[]) => {
        statManyLlamadas += 1;
        return rutas.map(() => ({
          size: 12,
          mtimeMs: 1_700_000_000_000,
          isFile: true,
          isDirectory: false,
        }));
      },
    } as unknown as FileSystemPort;

    const entradas = await escanearVault(port, CLAVES);

    expect(statManyLlamadas).toBe(1); // UN solo round-trip para los 3
    expect(statsIndividuales).toBe(0); // ni un stat suelto
    expect(entradas).toHaveLength(3);
  });
});

describe('un fallo de lectura no es un borrado', () => {
  it('el archivo ilegible queda fuera del diff y NO se propone del', async () => {
    const rutas = ['a.md', 'b.md', 'c.md'];
    const port = portDeArbol(rutas) as unknown as FileSystemPort & {
      readMany: (rels: string[]) => Promise<Array<Uint8Array | null>>;
    };
    // b.md existe (está en el listado) pero el read falla como un EACCES/EIO real.
    port.readMany = async (rels) =>
      rels.map((rel) => (rel === 'b.md' ? null : new TextEncoder().encode(`contenido de ${rel}`)));

    const ilegibles = new Map<string, string>();
    const entradas = await escanearVault(port, CLAVES, '', undefined, undefined, ilegibles);

    expect(entradas.map((entrada) => entrada.path)).toEqual(['a.md', 'c.md']);
    expect([...ilegibles.values()]).toEqual(['b.md']);

    const base = rutas.map((ruta) => ({
      pathId: toHex(calcularPathId(CLAVES, ruta)),
      path: ruta,
      contentHash: toHex(calcularPathId(CLAVES, `hash-${ruta}`)),
      size: 1,
      mtime: 1,
    }));
    const store = {
      listBase: async () => base,
      listIndex: async () => base.map((entrada) => ({ ...entrada, deleted: false })),
      listOutbox: async () => [],
    } as unknown as StateStore;

    const conMapa = await diffLocal(store, entradas, ilegibles);
    expect(conMapa.ilegibles).toEqual(['b.md']);
    expect(conMapa.borrados).toEqual([]);

    // Control: sin el mapa, el mismo escaneo SÍ borraría b.md (el bug que se arregló).
    const sinMapa = await diffLocal(store, entradas);
    expect(sinMapa.borrados.map((borrado) => borrado.path)).toEqual(['b.md']);
  });
});

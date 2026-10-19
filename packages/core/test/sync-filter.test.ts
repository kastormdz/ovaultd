import { describe, expect, it } from 'vitest';

import { esConfigPortable, estaExcluido, motivoDeExclusion } from '../src/SyncFilter.js';

describe('SyncFilter', () => {
  const excluidos: Array<[string, string]> = [
    ['.trash/nota-vieja.md', 'papelera'],
    ['.git/config', 'git'],
    ['proyecto/node_modules/lib/index.js', 'dependencias'],
    ['Notas/.ovaultd-tmp-abc1234567890def', 'temporal'],
    ['.ovaultd-tmp-92966fd551ea05a1', 'temporal'],
    ['workspace.json', 'workspace por dispositivo'],
    ['Carpeta/workspace-mobile.json', 'workspace por dispositivo'],
    // De configDir: lo que NO está en la lista blanca.
    ['.obsidian/workspace.json', 'workspace por dispositivo'],
    ['.obsidian/app.json', 'fuera de la lista blanca'],
    ['.obsidian/graph.json', 'fuera de la lista blanca'],
    // La lista de habilitados depende de qué plugins están INSTALADOS en cada dispositivo:
    // sincronizarla generaba conflictos de bajo valor (cada toggle la reescribe).
    ['.obsidian/community-plugins.json', 'fuera de la lista blanca'],
    ['.obsidian/plugins/comunidad/data.json', 'fuera de la lista blanca'],
    ['.obsidian/plugins/ovaultd/data.json', 'fuera de la lista blanca'],
    ['.obsidian/plugins/ovaultd/estado-local.json', 'fuera de la lista blanca'],
    ['.obsidian/plugins/ovaultd/sesion-cuenta.json', 'fuera de la lista blanca'],
    ['.obsidian/plugins/ovaultd/motor.log', 'fuera de la lista blanca'],
    ['.obsidian/plugins/ovaultd/main.js', 'fuera de la lista blanca'],
    // La identidad del propio plugin de sync es local: la versión la mueve la tienda.
    ['.obsidian/plugins/ovaultd/manifest.json', 'la tienda'],
    // Estado del DISPOSITIVO: Obsidian los reescribe solo y cada device los tiene distinto
    // (medido en producción: appearance.json acumuló 25 revisiones entre 3 dispositivos).
    ['.obsidian/appearance.json', 'fuera de la lista blanca'],
    ['.obsidian/core-plugins.json', 'fuera de la lista blanca'],
    ['.obsidian/hotkeys.json', 'fuera de la lista blanca'],
    ['.obsidian/plugins/comunidad/manifest.json', 'fuera de la lista blanca'],
  ];

  for (const [path, aguja] of excluidos) {
    it(`excluye ${path} (${aguja})`, () => {
      expect(estaExcluido(path)).toBe(true);
      expect(motivoDeExclusion(path)).toContain(aguja);
    });
  }

  // La lista blanca: lo único de configDir que viaja a los otros dispositivos. Es
  // CONTENIDO (CSS y sus manifiestos). Los JSON de estado del dispositivo quedaron afuera.
  const configPortable = [
    '.obsidian/snippets/mi-estilo.css',
    '.obsidian/themes/Obsidianite/theme.css',
    '.obsidian/themes/Obsidianite/manifest.json',
  ];

  for (const path of configPortable) {
    it(`deja pasar la config portable ${path}`, () => {
      expect(estaExcluido(path)).toBe(false);
      expect(motivoDeExclusion(path)).toBeNull();
    });
  }

  it('el configDir del vault puede no llamarse .obsidian', () => {
    // Un vault con el configDir cambiado: la política es la misma adentro.
    expect(estaExcluido('mi-config/appearance.json', 'mi-config')).toBe(true); // estado del device
    expect(estaExcluido('mi-config/themes/x/theme.css', 'mi-config')).toBe(false);
    expect(estaExcluido('mi-config/plugins/x/data.json', 'mi-config')).toBe(true);
    // Y un `.obsidian/` anidado en una carpeta común NO es configDir del vault.
    expect(estaExcluido('Carpeta/.obsidian/appearance.json', 'mi-config')).toBe(false);
  });

  it('la lista blanca es explícita: esConfigPortable no acepta prefijos', () => {
    expect(esConfigPortable('appearance.json')).toBe(false); // estado del dispositivo
    expect(esConfigPortable('appearance-otro.json')).toBe(false);
    expect(esConfigPortable('themes/x/theme.css')).toBe(true);
    expect(esConfigPortable('plugins/x/data.json')).toBe(false);
    expect(esConfigPortable('snippets/')).toBe(false); // un directorio vacío no aporta nada
  });

  const contenido = [
    'Notas/uno.md',
    'diario/2026-10-17.md',
    'adjuntos/imagen.png',
    '.notas-ocultas.md',
    'carpeta.d/nota.md',
    'mi-node_modules-nota.md',
    'workspace-teoria.md',
    '.obsidianite/nota.md',
  ];

  for (const path of contenido) {
    it(`deja pasar ${path}`, () => {
      expect(estaExcluido(path)).toBe(false);
      expect(motivoDeExclusion(path)).toBeNull();
    });
  }

  // Los CONTENEDORES del configDir se recursan: una carpeta excluida no se escanea (ni se
  // baja a ella), así que excluir la raíz o las carpetas de la lista blanca dejaba su
  // contenido portable inalcanzable — el bug que dejó de sincronizar los themes.
  const contenedores = [
    '.obsidian',
    '.obsidian/themes',
    '.obsidian/snippets',
    '.obsidian/plugins',
    '.obsidian/plugins/dataview',
  ];
  for (const path of contenedores) {
    it(`no excluye el contenedor ${path}`, () => {
      expect(estaExcluido(path)).toBe(false);
      expect(motivoDeExclusion(path)).toBeNull();
    });
  }

  it('lo NO portable del configDir sigue excluido (los contenedores no filtran vetados)', () => {
    expect(motivoDeExclusion('.obsidian/data.json')).toContain('fuera de la lista blanca');
    expect(motivoDeExclusion('.obsidian/plugins/dataview/data.json')).toContain(
      'fuera de la lista blanca',
    );
    expect(motivoDeExclusion('.obsidian/plugins/dataview/sub/cache.bin')).toContain(
      'fuera de la lista blanca',
    );
    expect(motivoDeExclusion('.obsidian/escenario.json')).toContain('fuera de la lista blanca');
  });
});

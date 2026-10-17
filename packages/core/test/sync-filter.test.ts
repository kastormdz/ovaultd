import { describe, expect, it } from 'vitest';

import { estaExcluido, motivoDeExclusion } from '../src/SyncFilter.js';

describe('SyncFilter', () => {
  const excluidos: Array<[string, string]> = [
    ['.obsidian/app.json', 'configuración del vault'],
    ['.obsidian/plugins/comunidad/data.json', 'configuración del vault'],
    ['.obsidian/themes/Obsidianite/theme.css', 'configuración del vault'],
    ['.trash/nota-vieja.md', 'papelera'],
    ['.git/config', 'git'],
    ['proyecto/node_modules/lib/index.js', 'dependencias'],
    ['Notas/.ovaultd-tmp-abc1234567890def', 'temporal'],
    ['.ovaultd-tmp-92966fd551ea05a1', 'temporal'],
    ['workspace.json', 'workspace por dispositivo'],
    ['Carpeta/workspace-mobile.json', 'workspace por dispositivo'],
  ];

  for (const [path, aguja] of excluidos) {
    it(`excluye ${path} (${aguja})`, () => {
      expect(estaExcluido(path)).toBe(true);
      expect(motivoDeExclusion(path)).toContain(aguja);
    });
  }

  const contenido = [
    'Notas/uno.md',
    'diario/2026-10-17.md',
    'adjuntos/imagen.png',
    '.notas-ocultas.md',
    'carpeta.d/nota.md',
    'mi-node_modules-nota.md',
    'workspace-teoria.md',
  ];

  for (const path of contenido) {
    it(`deja pasar ${path}`, () => {
      expect(estaExcluido(path)).toBe(false);
      expect(motivoDeExclusion(path)).toBeNull();
    });
  }
});

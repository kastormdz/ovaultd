/**
 * Escaneo + hash-diff (fase 2, paso 4) sobre vault REAL + runtime REAL.
 *
 *  - `sync.scan` lista archivos con `path_id`/hash y clasifica contra la base.
 *  - Lo aplicado desde el remoto no se re-encola (hash-diff, no mtime-diff).
 *  - modificación → `modificado`; borrado → `borrado`; sin tocar → `sinCambios`.
 *  - Los temporales `.ovaultd-tmp-*` del apply JAMÁS entran al escaneo.
 */

import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { expect, test } from '@playwright/test';

import { deriveSubkeys, encryptBlob, randomSalt, randomVaultKey, blobId } from '@ovaultd/crypto';

import { BrowserRuntime } from '../../packages/cli/src/browser-runtime';

const hex = (b: Uint8Array): string => Buffer.from(b).toString('hex');
const b64 = (b: Uint8Array): string => Buffer.from(b).toString('base64');
const sha = (d: Uint8Array | string): string => createHash('sha256').update(d).digest('hex');

const subkeys = deriveSubkeys(randomVaultKey(), randomSalt());
const CLAVES_HEX = {
  content: hex(subkeys.content),
  nonce: hex(subkeys.nonce),
  path: hex(subkeys.path),
  meta: hex(subkeys.meta),
  bind: hex(subkeys.bind),
  sign: hex(subkeys.sign),
};

interface ScanResult {
  diff: {
    nuevos: Array<{ path: string; pathId: string }>;
    modificados: Array<{ path: string; pathId: string }>;
    borrados: Array<{ pathId: string; path?: string }>;
    sinCambios: number;
    yaEncolados: number;
  };
  entradas: Array<{ path: string; pathId: string; size: number; contentHash: string }>;
}

test.describe('escaneo y hash-diff', () => {
  test('clasifica nuevos/modificados/borrados contra la base y saltea temporales', async () => {
    const vault = mkdtempSync(join(tmpdir(), 'ovaultd-scan-'));
    const dataDir = mkdtempSync(join(tmpdir(), 'ovaultd-rt-'));
    mkdirSync(join(vault, 'Notas'));

    const contenidoUno = new TextEncoder().encode('# uno\n');
    const contenidoDos = new TextEncoder().encode('# dos\n');
    writeFileSync(join(vault, 'Notas/uno.md'), contenidoUno);
    writeFileSync(join(vault, 'Notas/dos.md'), contenidoDos);
    // Temporal del apply: existe en el vault y NO puede aparecer en el escaneo.
    writeFileSync(join(vault, 'Notas/.ovaultd-tmp-abc'), 'basura');

    const runtime = await BrowserRuntime.open({ dataDir, port: 18_951, vaultRoot: vault });
    try {
      await runtime.call('store.open', { name: 'scan' });
      await runtime.call('apply.configure', { keys: CLAVES_HEX });

      // 1. Todo nuevo, temporales afuera, hashes correctos.
      const primero = (await runtime.call('sync.scan', {})) as ScanResult;
      expect(primero.diff.nuevos.length).toBe(2);
      expect(primero.diff.modificados.length).toBe(0);
      expect(primero.diff.borrados.length).toBe(0);
      expect(primero.diff.sinCambios).toBe(0);
      expect(primero.diff.yaEncolados).toBe(0);
      expect(primero.entradas.map((e) => e.path)).toEqual(['Notas/dos.md', 'Notas/uno.md']);
      const uno = primero.entradas.find((e) => e.path === 'Notas/uno.md')!;
      expect(uno.contentHash).toBe(sha(contenidoUno));
      expect(uno.size).toBe(contenidoUno.length);

      // 2. Aplicar una revisión remota de uno.md (mismo contenido) ⇒ pasa a sinCambios.
      const stored = encryptBlob(subkeys, contenidoUno);
      await runtime.call('apply.run', {
        revision: {
          pathId: uno.pathId,
          path: 'Notas/uno.md',
          kind: 'file',
          deleted: false,
          rev: 1,
          blobId: hex(blobId(stored)),
          contentHash: sha(contenidoUno),
        },
        storedB64: b64(stored),
      });

      const segundo = (await runtime.call('sync.scan', {})) as ScanResult;
      expect(segundo.diff.sinCambios).toBe(1);
      expect(segundo.diff.nuevos.length).toBe(1); // dos.md sigue nuevo

      // 3. Modificar uno.md ⇒ modificado (hash-diff, no mtime).
      writeFileSync(join(vault, 'Notas/uno.md'), '# uno editado\n');
      const tercero = (await runtime.call('sync.scan', {})) as ScanResult;
      expect(tercero.diff.modificados.length).toBe(1);
      expect(tercero.diff.sinCambios).toBe(0);

      // 4. Borrar dos.md ⇒ nada que emitir: nunca tuvo base (jamás se subió).
      unlinkSync(join(vault, 'Notas/dos.md'));
      const cuarto = (await runtime.call('sync.scan', {})) as ScanResult;
      expect(cuarto.diff.borrados.length).toBe(0);
      expect(cuarto.diff.modificados.length).toBe(1);

      // 5. El temporaral sigue invisible y un archivo idéntico al de la base no se re-encola.
      expect(cuarto.entradas.map((e) => e.path)).toEqual(['Notas/uno.md']);
    } finally {
      await runtime.close();
      rmSync(vault, { recursive: true, force: true });
      rmSync(dataDir, { recursive: true, force: true });
    }
  });
});

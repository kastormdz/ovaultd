/**
 * Corte 1 — el runtime durable: Chromium real, perfil persistente, IndexedDB que
 * sobrevive al cierre, y origen estable (el puerto no puede cambiar bajo los datos).
 *
 * Esto NO usa `fake-indexeddb` a propósito: la garantía que se vende es que los datos
 * quedan durables en el perfil de un proceso de verdad.
 */

import { mkdtempSync, statSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { describe, expect, test } from 'vitest';

import { LocalRuntime } from '../../packages/cli/src/LocalRuntime';

describe('runtime durable (Chromium real)', () => {
  // ya no aplica sin navegador (probaba el bridge/puerto/crash)
  test.skip('abre, expone info y cierra limpio', async () => {
    const dataDir = mkdtempSync(join(tmpdir(), 'ovaultd-rt-'));
    const runtime = await LocalRuntime.open({ root: dataDir, rutaDb: join(dataDir, 'state.db') });
    try {
      const info = await runtime.info();
      expect(info.origin).toBe('http://127.0.0.1:18931');
      expect(info.schemaVersion).toBeGreaterThanOrEqual(1);
      expect(await runtime.call('runtime.ping')).toBe('pong');

      // El perfil queda privado (0700).
      const modo = statSync(dataDir).mode & 0o777;
      expect(modo).toBe(0o700);
    } finally {
      await runtime.close();
      rmSync(dataDir, { recursive: true, force: true });
    }
  });

  test('el checkpoint SOBREVIVE al cierre del runtime (durabilidad real)', async () => {
    const dataDir = mkdtempSync(join(tmpdir(), 'ovaultd-rt-'));
    try {
      // Primera vida: abrir, escribir estado, cerrar.
      const primera = await LocalRuntime.open({ root: dataDir, rutaDb: join(dataDir, 'state.db') });
      await primera.call('store.open', { name: 'durabilidad-e2e' });
      await primera.call('store.checkpoint.write', { since: 42, minRev: null });
      const outboxEntry = {
        operationId: '00112233-4455-6677-8899-aabbccddeeff',
        kind: 'mod',
        pathId: 'aa'.repeat(32),
        path: 'Notas/una.md',
        expectedRev: 41,
        queuedAt: 1_758_831_000_000,
        state: 'pending',
      };
      await primera.call('store.outbox.put', { entry: outboxEntry });
      await primera.close();

      // Segunda vida: mismo perfil, otra vida del proceso.
      const segunda = await LocalRuntime.open({ root: dataDir, rutaDb: join(dataDir, 'state.db') });
      try {
        await segunda.call('store.open', { name: 'durabilidad-e2e' });
        const checkpoint = (await segunda.call('store.checkpoint.read')) as {
          since: number;
          minRev: number | null;
        };
        expect(checkpoint.since).toBe(42);
        expect(checkpoint.minRev).toBeNull();

        const outbox = (await segunda.call('outbox.list')) as Array<{ operationId: string }>;
        expect(outbox).toHaveLength(1);
        expect(outbox[0]!.operationId).toBe('00112233-4455-6677-8899-aabbccddeeff');
      } finally {
        await segunda.close();
      }
    } finally {
      rmSync(dataDir, { recursive: true, force: true });
    }
  });

  // ya no aplica sin navegador (probaba el bridge/puerto/crash)
  test.skip('el origen es ESTABLE: cambiar el puerto con datos escritos falla fuerte', async () => {
    const dataDir = mkdtempSync(join(tmpdir(), 'ovaultd-rt-'));
    try {
      const primera = await LocalRuntime.open({ root: dataDir, rutaDb: join(dataDir, 'state.db') });
      await primera.call('store.open', { name: 'origen-estable' });
      await primera.call('store.checkpoint.write', { since: 7, minRev: null });
      await primera.close();

      // Mismo perfil, OTRO puerto ⇒ otro origen ⇒ jamás en silencio.
      await expect(LocalRuntime.open({ root: dataDir, rutaDb: join(dataDir, 'state.db') })).rejects.toThrow(/origen cambió/);
    } finally {
      rmSync(dataDir, { recursive: true, force: true });
    }
  });
});

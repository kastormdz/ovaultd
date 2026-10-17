/**
 * Corte 2 — outbox *save-first* y journal de aplicación.
 *
 * Lo que se prueba contra Chromium REAL:
 *  - El snapshot del contenido queda recuperable y verificado por hash ANTES de publicar.
 *  - El `operation_id` es determinista: reintento idéntico ⇒ mismo ID (idempotente).
 *  - La outbox y las intenciones sobreviven al cierre del runtime.
 *  - El commit de aplicación es atómico: intención + base + índice + checkpoint en un paso.
 *  - `remote_apply_preserves_unqueued_local_edit`: aplicar remoto NUNCA pisa la edición
 *    local que sigue encolada (la prueba declarada del plan — falla si se invierte el orden).
 */

import { createHash } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { expect, test } from '@playwright/test';

import { BrowserRuntime } from '../../packages/cli/src/browser-runtime';

const sha256hex = (texto: string): string => createHash('sha256').update(texto, 'utf8').digest('hex');
const b64 = (texto: string): string => Buffer.from(texto, 'utf8').toString('base64');

const PATH_1 = 'Notas/una.md';
const PATH_ID_1 = 'aa'.repeat(32);
const CONTENIDO_V1 = '# nota\ncontenido local v1\n';
const HASH_V1 = sha256hex(CONTENIDO_V1);

test.describe('outbox save-first', () => {
  test('el snapshot viaja verificado y el operation_id es estable por payload', async () => {
    const dataDir = mkdtempSync(join(tmpdir(), 'ovaultd-ob-'));
    const runtime = await BrowserRuntime.open({ dataDir, port: 18_941 });
    try {
      await runtime.call('store.open', { name: 'outbox-spec' });

      const request = {
        kind: 'mod',
        path: PATH_1,
        pathId: PATH_ID_1,
        expectedRev: 5,
        contentB64: b64(CONTENIDO_V1),
        contentHash: HASH_V1,
      };

      const primera = (await runtime.call('outbox.enqueue', { request })) as {
        created: boolean;
        entry: { operationId: string; state: string; snapshotSize: number };
      };
      expect(primera.created).toBe(true);
      expect(primera.entry.operationId).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
      expect(primera.entry.state).toBe('pending');
      expect(primera.entry.snapshotSize).toBe(Buffer.byteLength(CONTENIDO_V1, 'utf8'));

      // Mismo payload ⇒ mismo ID, sin duplicar.
      const repetida = (await runtime.call('outbox.enqueue', { request })) as { created: boolean; entry: { operationId: string } };
      expect(repetida.created).toBe(false);
      expect(repetida.entry.operationId).toBe(primera.entry.operationId);

      // Rebase (precondición nueva) ⇒ operación NUEVA.
      const rebase = (await runtime.call('outbox.enqueue', { request: { ...request, expectedRev: 6 } })) as {
        created: boolean;
        entry: { operationId: string };
      };
      expect(rebase.created).toBe(true);
      expect(rebase.entry.operationId).not.toBe(primera.entry.operationId);

      // Hash mentiroso ⇒ se rechaza el encolado (el snapshot tiene que ser fiel).
      await expect(
        runtime.call('outbox.enqueue', { request: { ...request, contentHash: 'ff'.repeat(32) } }),
      ).rejects.toThrow(/no coincide/);

      // Un `del` no lleva contenido y un `move` exige prevPathId.
      await expect(
        runtime.call('outbox.enqueue', { request: { kind: 'del', path: PATH_1, pathId: PATH_ID_1, expectedRev: 7, contentB64: b64('x'), contentHash: sha256hex('x') } }),
      ).rejects.toThrow(/no lleva contenido/);
      await expect(
        runtime.call('outbox.enqueue', { request: { kind: 'move', path: 'Notas/dos.md', pathId: 'bb'.repeat(32), expectedRev: 7 } }),
      ).rejects.toThrow(/prevPathId/);
    } finally {
      await runtime.close();
      rmSync(dataDir, { recursive: true, force: true });
    }
  });

  test('la outbox y su snapshot SOBREVIVEN al cierre REAL del runtime', async () => {
    const dataDir = mkdtempSync(join(tmpdir(), 'ovaultd-ob-'));
    try {
      const primera = await BrowserRuntime.open({ dataDir, port: 18_942 });
      await primera.call('store.open', { name: 'outbox-durable' });
      await primera.call('outbox.enqueue', {
        request: { kind: 'mod', path: PATH_1, pathId: PATH_ID_1, expectedRev: 5, contentB64: b64(CONTENIDO_V1), contentHash: HASH_V1 },
      });
      await primera.close();

      const segunda = await BrowserRuntime.open({ dataDir, port: 18_942 });
      try {
        await segunda.call('store.open', { name: 'outbox-durable' });
        const lista = (await segunda.call('outbox.list')) as Array<{ pathId: string; state: string }>;
        expect(lista).toHaveLength(1);
        expect(lista[0]!.pathId).toBe(PATH_ID_1);
        expect(lista[0]!.state).toBe('pending');

        const snapshot = (await segunda.call('outbox.snapshot', { operationId: (await segunda.call('outbox.list') as Array<{ operationId: string }>)[0]!.operationId })) as { b64: string; hash: string };
        expect(snapshot.b64).toBe(b64(CONTENIDO_V1));
        expect(snapshot.hash).toBe(HASH_V1);
      } finally {
        await segunda.close();
      }
    } finally {
      rmSync(dataDir, { recursive: true, force: true });
    }
  });

  test('máquina de estados: pending → sent → confirmed → remove', async () => {
    const dataDir = mkdtempSync(join(tmpdir(), 'ovaultd-ob-'));
    const runtime = await BrowserRuntime.open({ dataDir, port: 18_943 });
    try {
      await runtime.call('store.open', { name: 'outbox-estados' });
      const { entry } = (await runtime.call('outbox.enqueue', {
        request: { kind: 'del', path: PATH_1, pathId: PATH_ID_1, expectedRev: 5 },
      })) as { entry: { operationId: string } };

      const sent = (await runtime.call('outbox.markSent', { operationId: entry.operationId })) as { state: string };
      expect(sent.state).toBe('sent');
      const confirmed = (await runtime.call('outbox.confirm', { operationId: entry.operationId })) as { state: string };
      expect(confirmed.state).toBe('confirmed');

      await expect(runtime.call('outbox.confirm', { operationId: 'no-existe' })).rejects.toThrow(/no existe/);

      await runtime.call('outbox.remove', { operationId: entry.operationId });
      expect(((await runtime.call('outbox.list')) as unknown[]).length).toBe(0);
    } finally {
      await runtime.close();
      rmSync(dataDir, { recursive: true, force: true });
    }
  });
});

test.describe('journal de aplicación', () => {
  test('el commit es atómico: intención + base + índice + checkpoint en un paso', async () => {
    const dataDir = mkdtempSync(join(tmpdir(), 'ovaultd-ap-'));
    const runtime = await BrowserRuntime.open({ dataDir, port: 18_944 });
    try {
      await runtime.call('store.open', { name: 'journal-spec' });

      const candidate = { pathId: PATH_ID_1, rev: 3, blobId: 'cc'.repeat(32) };
      const begin = (await runtime.call('apply.begin', { candidate })) as { created: boolean; intent: { intentId: string; state: string } };
      expect(begin.created).toBe(true);
      expect(begin.intent.state).toBe('pending');

      // Repetir el begin de la MISMA revisión no duplica.
      const begin2 = (await runtime.call('apply.begin', { candidate })) as { created: boolean; intent: { intentId: string } };
      expect(begin2.created).toBe(false);
      expect(begin2.intent.intentId).toBe(begin.intent.intentId);

      const verified = (await runtime.call('apply.verify', { intentId: begin.intent.intentId })) as { state: string };
      expect(verified.state).toBe('verified');

      const pendientes = (await runtime.call('apply.pending')) as Array<{ state: string }>;
      expect(pendientes).toHaveLength(1);
      expect(pendientes[0]!.state).toBe('verified');

      // Commit atómico.
      await runtime.call('apply.complete', {
        input: {
          intentId: begin.intent.intentId,
          base: { pathId: PATH_ID_1, rev: 3, blobId: 'cc'.repeat(32), contentHash: HASH_V1 },
          index: { pathId: PATH_ID_1, path: PATH_1, kind: 'file', deleted: false, rev: 3, blobId: 'cc'.repeat(32), contentHash: HASH_V1 },
          checkpoint: { since: 3, minRev: null },
        },
      });

      expect(((await runtime.call('apply.pending')) as unknown[]).length).toBe(0);
      const base = (await runtime.call('store.base.get', { pathId: PATH_ID_1 })) as { rev: number };
      const index = (await runtime.call('store.index.get', { pathId: PATH_ID_1 })) as { rev: number; contentHash: string };
      const checkpoint = (await runtime.call('store.checkpoint.read')) as { since: number };
      expect(base.rev).toBe(3);
      expect(index.rev).toBe(3);
      expect(index.contentHash).toBe(HASH_V1);
      expect(checkpoint.since).toBe(3);

      // Un intent inexistente no toca NADA.
      await expect(
        runtime.call('apply.complete', { input: { intentId: 'no-existe', checkpoint: { since: 99, minRev: null } } }),
      ).rejects.toThrow(/no existe la intención/);
      expect(((await runtime.call('store.checkpoint.read')) as { since: number }).since).toBe(3);
    } finally {
      await runtime.close();
      rmSync(dataDir, { recursive: true, force: true });
    }
  });

  test('remote_apply_preserves_unqueued_local_edit: el remoto NUNCA pisa la edición local encolada', async () => {
    const dataDir = mkdtempSync(join(tmpdir(), 'ovaultd-ap-'));
    const runtime = await BrowserRuntime.open({ dataDir, port: 18_945 });
    try {
      await runtime.call('store.open', { name: 'invariante-spec' });

      // Editás localmente: queda ENCOLADO (save-first) contra la base 5.
      const { entry } = (await runtime.call('outbox.enqueue', {
        request: { kind: 'mod', path: PATH_1, pathId: PATH_ID_1, expectedRev: 5, contentB64: b64(CONTENIDO_V1), contentHash: HASH_V1 },
      })) as { entry: { operationId: string } };

      // Llega un remoto para el MISMO path (rev 6) y se aplica.
      const candidate = { pathId: PATH_ID_1, rev: 6, blobId: 'dd'.repeat(32) };
      const begin = (await runtime.call('apply.begin', { candidate })) as { intent: { intentId: string } };
      await runtime.call('apply.complete', {
        input: {
          intentId: begin.intent.intentId,
          base: { pathId: PATH_ID_1, rev: 6, blobId: 'dd'.repeat(32) },
          index: { pathId: PATH_ID_1, path: PATH_1, kind: 'file', deleted: false, rev: 6, blobId: 'dd'.repeat(32) },
          checkpoint: { since: 6, minRev: null },
        },
      });

      // La edición local SIGUE encolada, intacta: el remoto no la pisa ni la borra.
      const enCola = (await runtime.call('outbox.list')) as Array<{ operationId: string; expectedRev: number; state: string; snapshotHash: string }>;
      expect(enCola).toHaveLength(1);
      expect(enCola[0]!.operationId).toBe(entry.operationId);
      expect(enCola[0]!.state).toBe('pending');
      expect(enCola[0]!.expectedRev).toBe(5);
      expect(enCola[0]!.snapshotHash).toBe(HASH_V1);

      // Y el snapshot se puede recuperar completo (el rebase lo necesita).
      const snapshot = (await runtime.call('outbox.snapshot', { operationId: entry.operationId })) as { b64: string };
      expect(snapshot.b64).toBe(b64(CONTENIDO_V1));

      // El checkpoint avanzó por el remoto: 6.
      expect(((await runtime.call('store.checkpoint.read')) as { since: number }).since).toBe(6);
    } finally {
      await runtime.close();
      rmSync(dataDir, { recursive: true, force: true });
    }
  });
});

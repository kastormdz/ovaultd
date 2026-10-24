/**
 * Corte 3 — puente de archivos confinado + orquestación de apply contra un vault REAL.
 *
 * Lo que se prueba contra Chromium REAL y filesystem REAL:
 *  - Confinamiento: travesía, rutas absolutas y symlinks se rechazan en el puente.
 *  - Apply feliz: intención → temporal verificado → publicar → commit atómico; sin residuos.
 *  - Integridad: blob_id mentiroso o content_hash distinto ⇒ no se publica NADA.
 *  - Tercera versión: una edición local posterior a la base JAMÁS se pisa (queda blocked).
 *  - Failpoints: SIGKILL real a Chromium entre verificar y publicar / después del rename /
 *    con una intención pending; la recuperación decide con hashes, no con fe.
 */

import { createHash, randomBytes } from 'node:crypto';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { expect, test } from '@playwright/test';

import { blobId, deriveSubkeys, encryptBlob, randomSalt, randomVaultKey } from '@ovaultd/crypto';

import { BrowserRuntime } from '../../packages/cli/src/browser-runtime';

const hex = (b: Uint8Array): string => Buffer.from(b).toString('hex');
const b64 = (b: Uint8Array): string => Buffer.from(b).toString('base64');
const sha = (d: Uint8Array | string): string => createHash('sha256').update(d).digest('hex');

// Un juego de subclaves para toda la suite (espejo local de lo que en producción
// deriva el CLI desde el material del vault).
const subkeys = deriveSubkeys(randomVaultKey(), randomSalt());
const CLAVES_HEX = {
  content: hex(subkeys.content),
  nonce: hex(subkeys.nonce),
  path: hex(subkeys.path),
  meta: hex(subkeys.meta),
  bind: hex(subkeys.bind),
  sign: hex(subkeys.sign),
};

interface Revision {
  pathId: string;
  path: string;
  kind: 'file';
  deleted: false;
  rev: number;
  blobId: string;
  contentHash: string;
}

const revisionDe = (path: string, plaintext: Uint8Array, rev: number, pathId?: string) => {
  const stored = encryptBlob(subkeys, plaintext);
  const revision: Revision = {
    pathId: pathId ?? hex(randomBytes(16)),
    path,
    kind: 'file',
    deleted: false,
    rev,
    blobId: hex(blobId(stored)),
    contentHash: sha(plaintext),
  };
  return { stored, revision };
};

const nuevoVault = (): string => mkdtempSync(join(tmpdir(), 'ovaultd-vault-'));

const abrir = async (port: number, vault: string) => {
  const dataDir = mkdtempSync(join(tmpdir(), 'ovaultd-rt-'));
  const runtime = await BrowserRuntime.open({ dataDir, port, vaultRoot: vault });
  await runtime.call('store.open', { name: 'apply-vault' });
  await runtime.call('apply.configure', { keys: CLAVES_HEX });
  return { runtime, dataDir };
};

test.describe('puente de archivos (confinamiento)', () => {
  test('travesía, absolutas y symlinks se rechazan; el vault sigue usable', async () => {
    const vault = nuevoVault();
    const fuera = mkdtempSync(join(tmpdir(), 'ovaultd-fuera-'));
    writeFileSync(join(fuera, 'secreto.txt'), 'no debe leerse');
    symlinkSync(fuera, join(vault, 'evil'));
    const { runtime, dataDir } = await abrir(18_946, vault);
    try {
      const fsCall = async (op: string, params: Record<string, unknown>) => {
        const r = await fetch(`http://127.0.0.1:18946/fs/${op}`, {
          method: 'POST',
          headers: { 'content-type': 'application/json', 'x-ovaultd-bridge': runtime.bridgeToken },
          body: JSON.stringify(params),
        });
        return r.json() as Promise<{ ok: boolean; result?: unknown; error?: { message: string } }>;
      };

      expect((await fsCall('stat', { path: '../x' })).ok).toBe(false);
      expect((await fsCall('stat', { path: 'a/../../x' })).ok).toBe(false);
      expect((await fsCall('stat', { path: '/etc/passwd' })).ok).toBe(false);
      expect((await fsCall('read', { path: 'evil/secreto.txt' })).ok).toBe(false);
      expect((await fsCall('stat', { path: 'evil' })).ok).toBe(false); // symlink final
      expect((await fsCall('writeTemp', { dir: 'evil', b64: b64(new Uint8Array([1])) })).ok).toBe(false);
      expect((await fsCall('mkdirp', { path: 'evil/sub' })).ok).toBe(false);
      expect((await fsCall('rename', { from: '../x', to: 'y' })).ok).toBe(false);
      expect((await fsCall('operacion-inventada', {})).ok).toBe(false);

      // Camino válido: leer y stat funcionan
      writeFileSync(join(vault, 'ok.txt'), 'hola vault');
      const lectura = (await fsCall('read', { path: 'ok.txt' })) as { ok: boolean; result: { b64: string } };
      expect(lectura.ok).toBe(true);
      expect(Buffer.from(lectura.result.b64, 'base64').toString('utf8')).toBe('hola vault');
      const stat = (await fsCall('stat', { path: 'ok.txt' })) as { ok: boolean; result: { size: number } };
      expect(stat.result.size).toBe(10);

      // El resto del servidor no expone nada más
      const otra = await fetch('http://127.0.0.1:18946/otra-cosa');
      expect(otra.status).toBe(404);
    } finally {
      await runtime.close();
      rmSync(vault, { recursive: true, force: true });
      rmSync(fuera, { recursive: true, force: true });
      rmSync(dataDir, { recursive: true, force: true });
    }
  });
});

test.describe('orquestación de apply', () => {
  test('apply feliz: temporal verificado → publicar → commit atómico, sin residuos', async () => {
    const vault = nuevoVault();
    const { runtime, dataDir } = await abrir(18_947, vault);
    try {
      const contenido = new TextEncoder().encode('# nota\nhola mundo\n');
      const { stored, revision } = revisionDe('Notas/uno.md', contenido, 1);

      const out = (await runtime.call('apply.run', { revision, storedB64: b64(stored) })) as { status: string };
      expect(out.status).toBe('applied');

      expect(readFileSync(join(vault, 'Notas/uno.md'))).toEqual(Buffer.from(contenido));
      expect(readdirSync(join(vault, 'Notas'))).toEqual(['uno.md']); // sin temporales

      const base = (await runtime.call('store.base.get', { pathId: revision.pathId })) as { rev: number; contentHash: string };
      expect(base.rev).toBe(1);
      expect(base.contentHash).toBe(revision.contentHash);
      // El cursor NO se mueve en el apply: lo cierra `pullOnce` al terminar la revisión
      // completa (una rev trae N ops; avanzar por registro dejaba el resto inalcanzable).
      expect(await runtime.call('store.checkpoint.read')).toBeNull();
      expect(await runtime.call('apply.pending')).toEqual([]);

      // Re-aplicar la misma revisión: el contenido local ya coincide ⇒ commit sin reescribir
      const deNuevo = (await runtime.call('apply.run', { revision, storedB64: b64(stored) })) as { status: string };
      expect(deNuevo.status).toBe('applied');
      expect(readdirSync(join(vault, 'Notas'))).toEqual(['uno.md']);

      // blob_id mentiroso: NADA se toca
      const v2 = revisionDe('Notas/dos.md', new TextEncoder().encode('x'), 1);
      const roto = Uint8Array.from(v2.stored);
      roto[0] ^= 0xff;
      await expect(
        runtime.call('apply.run', { revision: v2.revision, storedB64: b64(roto) }),
      ).rejects.toThrow(/blob_id no coincide/);
      expect(existsSync(join(vault, 'Notas/dos.md'))).toBe(false);

      // content_hash distinto: tampoco
      const v3 = revisionDe('Notas/tres.md', new TextEncoder().encode('y'), 1);
      await expect(
        runtime.call('apply.run', { revision: { ...v3.revision, contentHash: 'ff'.repeat(32) }, storedB64: b64(v3.stored) }),
      ).rejects.toThrow(/content_hash no coincide/);
      expect(existsSync(join(vault, 'Notas/tres.md'))).toBe(false);
      expect(await runtime.call('apply.pending')).toEqual([]);
    } finally {
      await runtime.close();
      rmSync(vault, { recursive: true, force: true });
      rmSync(dataDir, { recursive: true, force: true });
    }
  });

  test('tercera versión: la edición local posterior a la base se CONSERVA en una copia', async () => {
    const vault = nuevoVault();
    const { runtime, dataDir } = await abrir(18_948, vault);
    try {
      const v1 = revisionDe('Notas/uno.md', new TextEncoder().encode('remoto v1\n'), 1);
      await runtime.call('apply.run', { revision: v1.revision, storedB64: b64(v1.stored) });

      // Edición LOCAL después de la base:
      writeFileSync(join(vault, 'Notas/uno.md'), 'edición local\n');

      // Llega la rev 2 remota para el MISMO path:
      const v2 = revisionDe('Notas/uno.md', new TextEncoder().encode('remoto v2\n'), 2, v1.revision.pathId);
      const out = (await runtime.call('apply.run', { revision: v2.revision, storedB64: b64(v2.stored) })) as {
        status: string;
        copia?: string;
      };
      // Política por defecto (la de Obsidian Sync): el remoto va al path y la edición
      // local a una copia; NADA se pierde y el ciclo SIGUE.
      expect(out.status).toBe('conflicted-copy');
      expect(out.copia).toBeTruthy();

      expect(readFileSync(join(vault, 'Notas/uno.md'), 'utf8')).toBe('remoto v2\n');
      expect(readFileSync(join(vault, out.copia!), 'utf8')).toBe('edición local\n');
      // Sin nada bloqueado y el estado avanzó hasta la revisión aplicada:
      const pendientes = (await runtime.call('apply.pending')) as Array<{ state: string }>;
      expect(pendientes).toHaveLength(0);
      expect((await runtime.call('store.base.get', { pathId: v2.revision.pathId })) as { rev: number }).toHaveProperty('rev', 2);
      expect(await runtime.call('store.checkpoint.read')).toBeNull();
    } finally {
      await runtime.close();
      rmSync(vault, { recursive: true, force: true });
      rmSync(dataDir, { recursive: true, force: true });
    }
  });
});

test.describe('failpoints reales', () => {
  test('SIGKILL a Chromium: la recuperación decide con hashes (publica, limpia, commitea)', async () => {
    const vault = nuevoVault();
    const { runtime, dataDir } = await abrir(18_949, vault);
    let segunda: BrowserRuntime | null = null;
    try {
      mkdirSync(join(vault, 'Notas'), { recursive: true });

      // (a) verificada con temporal correcto, SIN publicar → recuperar debe publicar
      const ptA = new TextEncoder().encode('A\n');
      const tempA = 'Notas/.ovaultd-tmp-aaaa';
      writeFileSync(join(vault, tempA), ptA);
      const candA = {
        pathId: hex(randomBytes(16)),
        rev: 1,
        blobId: 'aa'.repeat(32),
        path: 'Notas/a.md',
        kind: 'file' as const,
        deleted: false,
        contentHash: sha(ptA),
      };
      const beginA = (await runtime.call('apply.begin', { candidate: candA })) as { intent: { intentId: string } };
      await runtime.call('apply.verify', { intentId: beginA.intent.intentId, tempName: tempA });

      // (b) pending sin temporal → se limpia
      const candB = {
        pathId: hex(randomBytes(16)),
        rev: 1,
        blobId: 'bb'.repeat(32),
        path: 'Notas/b.md',
        kind: 'file' as const,
        deleted: false,
        contentHash: sha('B'),
      };
      await runtime.call('apply.begin', { candidate: candB });

      // (c) después del rename: destino con el hash esperado y temporal ya consumido
      const ptC = new TextEncoder().encode('C\n');
      writeFileSync(join(vault, 'Notas/c.md'), ptC);
      const candC = {
        pathId: hex(randomBytes(16)),
        rev: 1,
        blobId: 'cc'.repeat(32),
        path: 'Notas/c.md',
        kind: 'file' as const,
        deleted: false,
        contentHash: sha(ptC),
      };
      const beginC = (await runtime.call('apply.begin', { candidate: candC })) as { intent: { intentId: string } };
      await runtime.call('apply.verify', { intentId: beginC.intent.intentId, tempName: 'Notas/.ovaultd-tmp-cccc' });

      // CRASH REAL: SIGKILL, sin flush ni close.
      const pid = await runtime.crash();
      expect(pid).toBeGreaterThan(0);

      // Reabrir el MISMO perfil (mismo origen) y recuperar:
      segunda = await BrowserRuntime.open({ dataDir, port: 18_949, vaultRoot: vault });
      await segunda.call('store.open', { name: 'apply-vault' });
      await segunda.call('apply.configure', { keys: CLAVES_HEX });
      const rep = (await segunda.call('apply.recover')) as { completed: number; cleaned: number; blocked: number };
      expect(rep).toEqual({ completed: 2, cleaned: 1, blocked: 0 });

      expect(readFileSync(join(vault, 'Notas/a.md'))).toEqual(Buffer.from(ptA)); // (a) publicada
      expect(readFileSync(join(vault, 'Notas/c.md'))).toEqual(Buffer.from(ptC)); // (c) sin tocar
      expect(existsSync(join(vault, 'Notas/b.md'))).toBe(false); // (b) nada
      expect(existsSync(join(vault, 'Notas/.ovaultd-tmp-aaaa'))).toBe(false); // temporal consumido
      expect(await segunda.call('apply.pending')).toEqual([]);
      const baseA = (await segunda.call('store.base.get', { pathId: candA.pathId })) as { rev: number };
      expect(baseA.rev).toBe(1);
      expect(await segunda.call('store.checkpoint.read')).toBeNull();
    } finally {
      if (segunda) await segunda.close();
      rmSync(vault, { recursive: true, force: true });
      rmSync(dataDir, { recursive: true, force: true });
    }
  });
});

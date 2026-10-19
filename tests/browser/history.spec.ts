/**
 * Fase 5 (historia) contra el stack REAL del contenedor, en Chromium REAL:
 *
 *  - `history.list`: las revisiones de un path (las SUYAS por `path_id`, la nueva primero).
 *  - `history.preview`: baja el blob de una revisión VIEJA del CAS y lo descifra — y el
 *    vault NO se toca (el archivo local sigue con la versión nueva).
 *  - Restaurar = escribir la versión vieja + sincronizar: sube como revisión NUEVA, firmada
 *    como cualquier `mod` — la historia jamás se reescribe.
 *
 * GUARDADO: corre solo desde `tests/scripts/container-e2e.sh` (OVAULTD_SYNC_E2E=1), después
 * del smoke (que sembró el vault con el cliente canónico en Rust). No asume NÚMEROS de
 * revisiones: los toma de las respuestas, así el orden de los specs no importa.
 */

import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { expect, test } from '@playwright/test';

import { decryptBlob, deriveSubkeys } from '@ovaultd/crypto';

import { BrowserRuntime } from '../../packages/cli/src/browser-runtime';

const E2E = process.env.OVAULTD_SYNC_E2E === '1';
const BASE = process.env.OVAULTD_SYNC_BASE ?? 'http://127.0.0.1:8888';
const ESTADO = process.env.OVAULTD_SYNC_STATE ?? '';

const hex = (b: Uint8Array): string => Buffer.from(b).toString('hex');

const pedirJson = async (ruta: string, init?: RequestInit): Promise<unknown> => {
  const respuesta = await fetch(`${BASE}${ruta}`, init);
  if (!respuesta.ok) {
    throw new Error(`${init?.method ?? 'GET'} ${ruta} → ${respuesta.status}: ${await respuesta.text()}`);
  }
  return respuesta.json();
};

interface ResumenRev {
  rev: number;
  op: string;
  size: number | null;
  blobId: string | null;
}

test.describe('historia E2E (stack real)', () => {
  test('list + preview (sin mutar) + restore como revisión nueva', async () => {
    test.skip(!E2E, 'se corre solo desde container-e2e.sh (OVAULTD_SYNC_E2E=1)');

    const estado = JSON.parse(readFileSync(ESTADO, 'utf8')) as {
      email: string;
      password: string;
      vault_key: string;
      vault_uid: string;
      salt: string;
    };

    const signin = (await pedirJson('/api/v1/auth/signin', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        email: estado.email,
        password: estado.password,
        name: 'e2e-history',
        platform: 'playwright',
      }),
    })) as { token: string; device_id: number };
    expect(signin.token).toBeTruthy();

    const vaults = (await pedirJson('/api/v1/vaults', {
      headers: { authorization: `Bearer ${signin.token}` },
    })) as Array<{ id: number }>;
    expect(vaults.length).toBeGreaterThan(0);
    const vault = vaults[0]!.id; // terreno del e2e: un solo vault

    const subkeys = deriveSubkeys(
      new Uint8Array(Buffer.from(estado.vault_key, 'base64')),
      new Uint8Array(Buffer.from(estado.salt, 'base64')),
    );
    const clavesHex = {
      content: hex(subkeys.content),
      nonce: hex(subkeys.nonce),
      path: hex(subkeys.path),
      meta: hex(subkeys.meta),
      bind: hex(subkeys.bind),
      sign: hex(subkeys.sign),
    };

    const vaultRoot = mkdtempSync(join(tmpdir(), 'ovaultd-hist-'));
    const dataDir = mkdtempSync(join(tmpdir(), 'ovaultd-hist-rt-'));
    const runtime = await BrowserRuntime.open({ dataDir, port: 18_951, vaultRoot });
    try {
      await runtime.call('store.open', { name: 'history-e2e' });
      await runtime.call('apply.configure', { keys: clavesHex });
      await runtime.call('sync.open', {
        baseUrl: BASE,
        token: signin.token,
        vault,
        device: signin.device_id,
        since: 0,
        vaultUuid: estado.vault_uid,
        epoch: 1,
      });
      const pull = (await runtime.call('sync.pullOnce')) as { status: string };
      expect(pull.status).toBe('pulled');

      // Nuestra nota con historia propia: v1 → v2, dos revisiones nuevas del vault.
      const v1 = new TextEncoder().encode('# Historia e2e\nprimera version\n');
      writeFileSync(join(vaultRoot, 'Notas/historia-e2e.md'), v1);
      const p1 = (await runtime.call('sync.pushOnce')) as {
        encoladas: number;
        empujadas: number;
        headRev: number;
        conflicto?: unknown;
      };
      expect(p1.conflicto).toBeUndefined();
      expect(p1.encoladas).toBe(1);
      expect(p1.empujadas).toBe(1);
      const revV1 = p1.headRev;

      const v2 = new TextEncoder().encode('# Historia e2e\nsegunda version\n');
      writeFileSync(join(vaultRoot, 'Notas/historia-e2e.md'), v2);
      const p2 = (await runtime.call('sync.pushOnce')) as { empujadas: number; headRev: number };
      expect(p2.empujadas).toBe(1);
      const revV2 = p2.headRev;
      expect(revV2).toBeGreaterThan(revV1);

      // ---------------------------------------------------------------- history.list
      const historial = (await runtime.call('history.list', {
        ruta: 'Notas/historia-e2e.md',
      })) as { items: ResumenRev[] };
      const mias = historial.items.filter((item) => item.rev === revV1 || item.rev === revV2);
      expect(mias.length).toBe(2, 'las dos revisiones del archivo están en la historia');
      expect(mias[0]!.rev).toBe(revV2);
      expect(mias[0]!.op).toBe('mod');
      expect(mias[1]!.rev).toBe(revV1);
      expect(mias[1]!.op).toBe('add');

      // ---------------------------------------------------------------- preview: NO muta
      const preview = (await runtime.call('history.preview', {
        blobId: mias[1]!.blobId,
      })) as { texto: string };
      expect(preview.texto).toBe('# Historia e2e\nprimera version\n');
      // El archivo del vault NO se tocó: sigue con la v2.
      expect(readFileSync(join(vaultRoot, 'Notas/historia-e2e.md'), 'utf8')).toBe(
        '# Historia e2e\nsegunda version\n',
      );

      // ---------------------------------------------------------------- restore = rev NUEVA
      // El restore del cliente es escribir la versión elegida y dejar que el scan la suba.
      writeFileSync(join(vaultRoot, 'Notas/historia-e2e.md'), new TextEncoder().encode(preview.texto));
      const p3 = (await runtime.call('sync.pushOnce')) as {
        encoladas: number;
        empujadas: number;
        headRev: number;
      };
      expect(p3.encoladas).toBe(1);
      expect(p3.empujadas).toBe(1);
      const revRestaurada = p3.headRev;
      expect(revRestaurada).toBeGreaterThan(revV2);

      // La historia del path ahora tiene TRES revisiones suyas: add, mod (v2), mod (restaurada).
      const historial2 = (await runtime.call('history.list', {
        ruta: 'Notas/historia-e2e.md',
      })) as { items: ResumenRev[] };
      const tres = historial2.items.filter(
        (item) => item.rev === revV1 || item.rev === revV2 || item.rev === revRestaurada,
      );
      expect(tres.length).toBe(3);
      expect(tres[0]!.rev).toBe(revRestaurada, 'la restaurada es la más nueva');
      expect(tres[0]!.op).toBe('mod');

      // Y su blob descifra a la v1: el server firmó y guardó EL CONTENIDO restaurado.
      // (La historia da el blob en HEX — a diferencia del delta, que lo manda en base64.)
      const hexa = tres[0]!.blobId!;
      const respuesta = await fetch(`${BASE}/api/v1/vaults/${vault}/blobs/${hexa}`, {
        headers: { authorization: `Bearer ${signin.token}` },
      });
      expect(respuesta.ok).toBe(true);
      const texto = decryptBlob(subkeys, new Uint8Array(await respuesta.arrayBuffer()));
      expect(Buffer.from(texto)).toEqual(Buffer.from(v1));

      await runtime.call('sync.close');
    } finally {
      await runtime.close();
      rmSync(vaultRoot, { recursive: true, force: true });
      rmSync(dataDir, { recursive: true, force: true });
    }
  });
});

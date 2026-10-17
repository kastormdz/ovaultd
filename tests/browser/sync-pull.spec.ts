/**
 * Corte 4 — sync E2E contra el stack REAL (contenedor + proxy reverso).
 *
 * GUARDADO: corre solo dentro de `tests/scripts/container-e2e.sh` (OVAULTD_SYNC_E2E=1),
 * después del smoke — que dejó dos revisiones firmadas por el smoke-cliente (el cliente
 * canónico en Rust) y el estado del vault. Se prueba, contra Chromium REAL:
 *
 *  - El upgrade del WS autenticado por SUBPROTOCOLO (ADR-0015): el navegador no puede
 *    fijar headers; si esto no anduviera, `sync.open` ni siquiera completaría el handshake.
 *  - La delta REAL del server se descifra y se aplica byte a byte al vault, con el
 *    checkpoint avanzando por `completeApply`.
 *  - Idempotencia: una segunda pasada no re-aplica nada.
 */

import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { expect, test } from '@playwright/test';

import { deriveSubkeys } from '@ovaultd/crypto';

import { BrowserRuntime } from '../../packages/cli/src/browser-runtime';

const E2E = process.env.OVAULTD_SYNC_E2E === '1';
const BASE = process.env.OVAULTD_SYNC_BASE ?? 'http://127.0.0.1:8888';
const ESTADO = process.env.OVAULTD_SYNC_STATE ?? '';
const SMOKE = process.env.OVAULTD_SYNC_SMOKE ?? '';

const hex = (b: Uint8Array): string => Buffer.from(b).toString('hex');

const pedirJson = async (ruta: string, init?: RequestInit): Promise<unknown> => {
  const respuesta = await fetch(`${BASE}${ruta}`, init);
  if (!respuesta.ok) {
    throw new Error(`${init?.method ?? 'GET'} ${ruta} → ${respuesta.status}: ${await respuesta.text()}`);
  }
  return respuesta.json();
};

test.describe('sync E2E (stack real)', () => {
  test('pull real: la delta del smoke-cliente se aplica byte a byte en Chromium', async () => {
    test.skip(!E2E, 'se corre solo desde container-e2e.sh (OVAULTD_SYNC_E2E=1)');

    const estado = JSON.parse(readFileSync(ESTADO, 'utf8')) as {
      email: string;
      password: string;
      vault_key: string;
      salt: string;
    };

    // Signin real (el estado trae la cuenta del smoke; la base es la del contenedor).
    const signin = (await pedirJson('/api/v1/auth/signin', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        email: estado.email,
        password: estado.password,
        name: 'e2e-browser',
        platform: 'playwright',
      }),
    })) as { token: string; device_id: number };
    expect(signin.token).toBeTruthy();

    const vaults = (await pedirJson('/api/v1/vaults', {
      headers: { authorization: `Bearer ${signin.token}` },
    })) as Array<{ id: number }>;
    expect(vaults.length).toBeGreaterThan(0);
    const vault = vaults[0]!.id; // terreno fresco del e2e: un solo vault

    // Subclaves del vault REAL: HKDF(vault_key, salt) — sin KDF de por medio (el KDF es
    // solo para envolver la vault_key, no para las subclaves).
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

    const vaultRoot = mkdtempSync(join(tmpdir(), 'ovaultd-sync-'));
    const dataDir = mkdtempSync(join(tmpdir(), 'ovaultd-rt-'));
    const runtime = await BrowserRuntime.open({ dataDir, port: 18_950, vaultRoot });
    try {
      await runtime.call('store.open', { name: 'sync-pull' });
      await runtime.call('apply.configure', { keys: clavesHex });

      // Este open ejercita el upgrade por subprotocolo (sin headers) + handshake completo.
      const abierto = (await runtime.call('sync.open', {
        baseUrl: BASE,
        token: signin.token,
        vault,
        device: signin.device_id,
        since: 0,
      })) as { headRev: number };
      expect(abierto.headRev).toBe(2);

      const primera = (await runtime.call('sync.pullOnce')) as { status: string; applied: number };
      expect(primera.status).toBe('pulled');
      expect(primera.applied).toBe(2);

      // Byte a byte contra los plaintext del smoke (lo que cifró el cliente en Rust):
      expect(readFileSync(join(vaultRoot, 'Notas/uno.md'))).toEqual(
        readFileSync(join(SMOKE, 'cliente/nota-1.txt')),
      );
      expect(readFileSync(join(vaultRoot, 'Notas/dos.md'))).toEqual(
        readFileSync(join(SMOKE, 'cliente/nota-2.txt')),
      );
      expect((await runtime.call('store.checkpoint.read')) as { since: number }).toHaveProperty(
        'since',
        2,
      );
      expect(await runtime.call('apply.pending')).toEqual([]);

      // Idempotente: nada nuevo, checkpoint intacto.
      const segunda = (await runtime.call('sync.pullOnce')) as { status: string; applied: number };
      expect(segunda).toEqual({ status: 'pulled', applied: 0, headRev: 2 });

      await runtime.call('sync.close');
    } finally {
      await runtime.close();
      rmSync(vaultRoot, { recursive: true, force: true });
      rmSync(dataDir, { recursive: true, force: true });
    }
  });
});

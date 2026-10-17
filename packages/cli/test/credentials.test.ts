/**
 * Credenciales y estado local (aceptación de fase 2: `credentials.spec.ts`).
 *
 * Lo que se prueba:
 *  - La password por `OVAULTD_PASSWORD` TIENE prioridad sobre cualquier prompt.
 *  - Sin TTY y sin env/flags, el prompt oculto falla ruidoso (jamás queda esperando).
 *  - El estado local se escribe 0600 (session) y 0700 (directorios), y `unlink` lo borra.
 */

import { mkdtempSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterAll, beforeEach, describe, expect, test } from 'vitest';

// configDir() resuelve el env en CADA llamada: dirigirlo a un tmpdir aísla el test.
const dir = mkdtempSync(join(tmpdir(), 'ovaultd-cli-test-'));
process.env.OVAULTD_CONFIG_DIR = dir;

import { resolvePassword } from '../src/credentials.js';
import {
  readSession,
  removeVaultState,
  saveSession,
  saveVaultState,
  vaultPath,
} from '../src/state.js';

const modo = (ruta: string): number => statSync(ruta).mode & 0o777;

beforeEach(() => {
  delete process.env.OVAULTD_PASSWORD;
});

afterAll(() => {
  rmSync(dir, { recursive: true, force: true });
});

describe('credenciales', () => {
  test('OVAULTD_PASSWORD tiene prioridad', async () => {
    process.env.OVAULTD_PASSWORD = 'password-de-prueba-env';
    await expect(resolvePassword({})).resolves.toBe('password-de-prueba-env');
  });

  test('sin TTY, env ni --password-stdin: falla ruidoso, no se cuelga', async () => {
    // En vitest no hay TTY: el prompt oculto tiene que rechazar con instrucciones.
    await expect(resolvePassword({})).rejects.toThrow(/TTY/);
  });
});

describe('estado local', () => {
  test('session.json queda 0600 y el directorio 0700', () => {
    saveSession({
      server: 'http://127.0.0.1:9',
      email: 'test@example.com',
      token: 'token-de-prueba',
      device_id: 1,
      account_id: 1,
      saved_at: new Date().toISOString(),
    });

    expect(modo(dir)).toBe(0o700);
    expect(modo(join(dir, 'session.json'))).toBe(0o600);
    expect(readSession()?.email).toBe('test@example.com');
  });

  test('el estado del vault queda 0600 y `unlink` lo borra', () => {
    saveVaultState({
      server: 'http://127.0.0.1:9',
      vault: 7,
      vault_id: 'AAAAAAAAAAAAAAAAAAAAAA==',
      name: 'prueba',
      role: 'owner',
      epoch: 1,
      salt: 'AAAAAAAAAAAAAAAAAAAAAA==',
      kdf_params: { m_kib: 8, t: 1, p: 1, dk_len: 32 },
      wrapped_key_pw: 'AAAA',
      wrapped_key_rc: 'AAAA',
      vault_pubkey: 'AAAA',
      dir: '/tmp/vault-de-prueba',
      saved_at: new Date().toISOString(),
    });

    const ruta = vaultPath('http://127.0.0.1:9', 7);
    expect(modo(ruta)).toBe(0o600);
    expect(modo(join(dir, 'vaults'))).toBe(0o700);

    expect(removeVaultState('http://127.0.0.1:9', 7)).toBe(true);
    expect(removeVaultState('http://127.0.0.1:9', 7)).toBe(false); // ya no está
  });
});

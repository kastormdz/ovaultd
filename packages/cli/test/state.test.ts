/**
 * Estado local: roundtrip, permisos 0600 y nombres de archivo por server+vault.
 */

import { mkdtempSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import {
  clearSession,
  configDir,
  listVaultStates,
  readSession,
  readVaultState,
  saveSession,
  saveVaultState,
  subclavesAHex,
  subclavesDeHex,
  vaultPath,
  type SessionState,
  type VaultLocalState,
} from '../src/state.js';

let dir: string;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), 'ovaultd-cli-'));
  process.env.OVAULTD_CONFIG_DIR = dir;
});

afterEach(() => {
  delete process.env.OVAULTD_CONFIG_DIR;
});

const sesion: SessionState = {
  server: 'http://127.0.0.1:8791',
  email: 'a@example.test',
  token: 'token-de-prueba',
  device_id: 3,
  account_id: 1,
  saved_at: '2026-10-16T00:00:00.000Z',
};

const vault: VaultLocalState = {
  server: 'http://127.0.0.1:8791',
  vault: 7,
  vault_id: 'AQIDBAUGBwgJCgsMDQ4PEA==',
  name: 'vault',
  role: 'owner',
  epoch: 1,
  salt: 'AAECAwQFBgcICQoLDA0ODw==',
  kdf_params: { m_kib: 65536, t: 3, p: 4, dk_len: 32 },
  wrapped_key_pw: 'ZW52dWVsdGE=',
  wrapped_key_rc: 'ZW52dWVsdGE=',
  vault_pubkey: 'cHVia2V5',
  saved_at: '2026-10-16T00:00:00.000Z',
};

describe('estado del CLI', () => {
  it('hace roundtrip de la sesión con permisos 0600', () => {
    saveSession(sesion);
    expect(readSession()).toEqual(sesion);

    const modo = statSync(join(configDir(), 'session.json')).mode & 0o777;
    expect(modo).toBe(0o600);
  });

  it('borra la sesión', () => {
    saveSession(sesion);
    clearSession();
    expect(readSession()).toBeNull();
  });

  it('nombra el archivo del vault por server+vault y hace roundtrip', () => {
    saveVaultState(vault);
    expect(vaultPath(vault.server, vault.vault)).toContain('127.0.0.1_8791_7.json');
    expect(readVaultState(vault.server, vault.vault)).toEqual(vault);

    const modo = statSync(vaultPath(vault.server, vault.vault)).mode & 0o777;
    expect(modo).toBe(0o600);
  });

  it('lista los vaults locales', () => {
    saveVaultState(vault);
    saveVaultState({ ...vault, vault: 8, name: 'otro' });
    const estados = listVaultStates();
    expect(estados.map((estado) => estado.vault).sort()).toEqual([7, 8]);
  });

  it('no guarda secretos en el estado del vault', () => {
    saveVaultState(vault);
    const crudo = readFileSync(vaultPath(vault.server, vault.vault), 'utf8');
    for (const prohibido of ['vault_key', 'vaultKey', 'password', 'recovery']) {
      expect(crudo.includes(prohibido)).toBe(false);
    }
  });

  it('sobrevive a un session.json corrupto con error claro', () => {
    saveSession(sesion);
    writeFileSync(join(configDir(), 'session.json'), '{no soy json');
    expect(() => readSession()).toThrow();
  });
});

describe('subclaves cacheadas', () => {
  it('round-trip hex y persisten con permisos 0600', () => {
    const subkeys = {
      content: new Uint8Array(32).fill(1),
      nonce: new Uint8Array(24).fill(2),
      path: new Uint8Array(32).fill(3),
      meta: new Uint8Array(32).fill(4),
      bind: new Uint8Array(32).fill(5),
      sign: new Uint8Array(32).fill(6),
    };
    const hex = subclavesAHex(subkeys as never);
    expect(subclavesDeHex(hex).content).toEqual(subkeys.content);
    expect(subclavesDeHex(hex).nonce).toEqual(subkeys.nonce);

    saveVaultState({ ...vault, subkeys: hex, subkeys_epoch: 1 });
    const leido = readVaultState(vault.server, vault.vault);
    expect(leido?.subkeys).toEqual(hex);
    expect(leido?.subkeys_epoch).toBe(1);
    expect(statSync(vaultPath(vault.server, vault.vault)).mode & 0o777).toBe(0o600);
  });
});

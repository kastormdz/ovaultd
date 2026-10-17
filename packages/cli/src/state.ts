/**
 * Estado local del CLI.
 *
 * - `session.json` (0600): server, email, token del device. La password jamás se guarda.
 * - `vaults/<server>_<vault>.json` (0600): material PÚBLICO del vault + dir vinculado.
 *   Ni la vault_key, ni la KEK, ni el secreto de recovery se persisten acá (regla E2E:
 *   lo único que queda en disco es la env del token, si el usuario la usa).
 */

import { chmodSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

import type { KdfParamsWire, VaultSummary } from '@ovaultd/protocol';

export interface SessionState {
  server: string;
  email: string;
  token: string;
  device_id: number;
  account_id: number;
  saved_at: string;
}

export interface VaultLocalState {
  server: string;
  /** id numérico del server. */
  vault: number;
  /** UUID de 16 B del cliente en base64 (ADR-0013). */
  vault_id: string;
  name: string;
  role: string;
  epoch: number;
  salt: string;
  kdf_params: KdfParamsWire;
  wrapped_key_pw: string;
  wrapped_key_rc: string;
  vault_pubkey: string;
  /** Carpeta local vinculada por `setup`, si existe. */
  dir?: string;
  saved_at: string;
}

export const configDir = (): string =>
  process.env.OVAULTD_CONFIG_DIR ?? join(homedir(), '.config', 'ovaultd');

const vaultsDir = (): string => join(configDir(), 'vaults');

export const sessionPath = (): string => join(configDir(), 'session.json');

const asegurarDir = (ruta: string): void => {
  mkdirSync(ruta, { recursive: true, mode: 0o700 });
  chmodSync(ruta, 0o700);
};

const escribirPrivado = (ruta: string, contenido: string): void => {
  writeFileSync(ruta, contenido, { mode: 0o600 });
  chmodSync(ruta, 0o600);
};

export const saveSession = (sesion: SessionState): void => {
  asegurarDir(configDir());
  escribirPrivado(sessionPath(), `${JSON.stringify(sesion, null, 2)}\n`);
};

export const readSession = (): SessionState | null => {
  const ruta = sessionPath();
  if (!existsSync(ruta)) return null;
  return JSON.parse(readFileSync(ruta, 'utf8')) as SessionState;
};

export const clearSession = (): void => {
  const ruta = sessionPath();
  if (existsSync(ruta)) rmSync(ruta);
};

const sanitizar = (servidor: string): string =>
  servidor.replace(/^https?:\/\//, '').replace(/[^a-zA-Z0-9._-]/g, '_');

export const vaultPath = (servidor: string, vault: number): string =>
  join(vaultsDir(), `${sanitizar(servidor)}_${vault}.json`);

export const saveVaultState = (estado: VaultLocalState): void => {
  asegurarDir(vaultsDir());
  escribirPrivado(vaultPath(estado.server, estado.vault), `${JSON.stringify(estado, null, 2)}\n`);
};

export const readVaultState = (servidor: string, vault: number): VaultLocalState | null => {
  const ruta = vaultPath(servidor, vault);
  if (!existsSync(ruta)) return null;
  return JSON.parse(readFileSync(ruta, 'utf8')) as VaultLocalState;
};

export const listVaultStates = (): VaultLocalState[] => {
  const dir = vaultsDir();
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((nombre) => nombre.endsWith('.json'))
    .map((nombre) => JSON.parse(readFileSync(join(dir, nombre), 'utf8')) as VaultLocalState);
};

/** Borra el estado local de un vault (`unlink`). Devuelve si existía. */
export const removeVaultState = (servidor: string, vault: number): boolean => {
  const ruta = vaultPath(servidor, vault);
  if (!existsSync(ruta)) return false;
  rmSync(ruta);
  return true;
};

export type { VaultSummary };

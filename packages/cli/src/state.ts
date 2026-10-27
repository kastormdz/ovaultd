/**
 * Estado local del CLI.
 *
 * - `session.json` (0600): server, email, token del device. La password jamás se guarda.
 * - `vaults/<server>_<vault>.json` (0600): material PÚBLICO del vault + dir vinculado.
 *   Ni la vault_key, ni la KEK, ni el secreto de recovery se persisten acá (regla E2E:
 *   lo único que queda en disco es la env del token, si el usuario la usa).
 */

import {
  chmodSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

import type { KdfParamsWire, VaultSummary } from '@ovaultd/protocol';
import { fromHex, toHex } from '@ovaultd/protocol';
import type { SubKeys } from '@ovaultd/crypto';

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
  /** Último conteo de notas (`.md`) visto por este cliente. */
  notes?: number;
  /** Carpeta local vinculada por `setup`, si existe. */
  dir?: string;
  /**
   * Subclaves derivadas cacheadas (hex): evitan pedir la password en cada `sync`
   * (un timer no puede tipearla). Mismo modelo que el plugin; el archivo es 0600 y
   * se borran con `ovaultd olvidar-clave`.
   */
  subkeys?: SubclavesEnHex;
  /** Epoch con la que se derivaron: si la vault_key rota, el cache queda inválido. */
  subkeys_epoch?: number;
  saved_at: string;
}

export interface SubclavesEnHex {
  content: string;
  nonce: string;
  path: string;
  meta: string;
  bind: string;
  sign: string;
}

/** Subclaves → hex para el estado local (lo único que se cachea). */
export const subclavesAHex = (subkeys: SubKeys): SubclavesEnHex => ({
  content: toHex(subkeys.content),
  nonce: toHex(subkeys.nonce),
  path: toHex(subkeys.path),
  meta: toHex(subkeys.meta),
  bind: toHex(subkeys.bind),
  sign: toHex(subkeys.sign),
});

/** Hex del estado local → subclaves (sin tocar la password). */
export const subclavesDeHex = (hex: SubclavesEnHex): SubKeys => ({
  content: fromHex(hex.content),
  nonce: fromHex(hex.nonce),
  path: fromHex(hex.path),
  meta: fromHex(hex.meta),
  bind: fromHex(hex.bind),
  sign: fromHex(hex.sign),
});

export const configDir = (): string =>
  process.env.OVAULTD_CONFIG_DIR ?? join(homedir(), '.config', 'ovaultd');

const vaultsDir = (): string => join(configDir(), 'vaults');

export const sessionPath = (): string => join(configDir(), 'session.json');

const asegurarDir = (ruta: string): void => {
  mkdirSync(ruta, { recursive: true, mode: 0o700 });
  chmodSync(ruta, 0o700);
};

/**
 * Escritura ATÓMICA: un corte de luz entre el truncado y el contenido dejaba el archivo a
 * medio escribir y el CLI arrancaba sin estado (o peor, con un `session.json` inválido).
 * Se escribe al lado y se renombra (mismo filesystem: el rename es atómico).
 */
const escribirPrivado = (ruta: string, contenido: string): void => {
  const temporal = `${ruta}.tmp-${process.pid}`;
  writeFileSync(temporal, contenido, { mode: 0o600 });
  chmodSync(temporal, 0o600);
  renameSync(temporal, ruta);
};

/**
 * Lee un JSON privado y devuelve `null` si está corrupto: un archivo inválido no puede
 * tumbar el CLI, se trata como "no hay estado" y se regenera.
 */
const leerJson = <T>(ruta: string): T | null => {
  try {
    return JSON.parse(readFileSync(ruta, 'utf8')) as T;
  } catch {
    return null;
  }
};

export const saveSession = (sesion: SessionState): void => {
  asegurarDir(configDir());
  escribirPrivado(sessionPath(), `${JSON.stringify(sesion, null, 2)}\n`);
};

export const readSession = (): SessionState | null => {
  const ruta = sessionPath();
  if (!existsSync(ruta)) return null;
  return leerJson<SessionState>(ruta);
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
  return leerJson<VaultLocalState>(ruta);
};

export const listVaultStates = (): VaultLocalState[] => {
  const dir = vaultsDir();
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((nombre) => nombre.endsWith('.json'))
    .map((nombre) => leerJson<VaultLocalState>(join(dir, nombre)))
    .filter((estado): estado is VaultLocalState => estado !== null);
};

/** Borra el estado local de un vault (`unlink`). Devuelve si existía. */
export const removeVaultState = (servidor: string, vault: number): boolean => {
  const ruta = vaultPath(servidor, vault);
  if (!existsSync(ruta)) return false;
  rmSync(ruta);
  return true;
};

export type { VaultSummary };

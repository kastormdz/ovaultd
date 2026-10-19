/**
 * Vinculación local → runtime durable (los comandos de la fase 2: `list-local`, `sync`).
 *
 * Acá SÍ se toca cripto: se desenvuelve la `vault_key` con la password del usuario
 * (KEK → unwrap; nunca se persiste) y se derivan las subclaves que el runtime usa para
 * cifrar/descifrar/firmar. El runtime corre con SU perfil Chromium por vault (origen
 * estable, datos IndexedDB propios) y su raíz de archivos es la carpeta vinculada.
 *
 * El token sale de `OVAULTD_TOKEN` si está (para CI/scripts; no se persiste aparte) o
 * del `session.json` (0600) del login.
 */

import { rmSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

import { deriveKek, deriveSubkeys, unwrapKeyPw, type SubKeys } from '@ovaultd/crypto';
import { b64Decode, toHex } from '@ovaultd/protocol';

import { BrowserRuntime } from './browser-runtime.js';
import { resolvePassword } from './credentials.js';
import {
  listVaultStates,
  readSession,
  readVaultState,
  saveVaultState,
  subclavesAHex,
  subclavesDeHex,
  type SessionState,
  type VaultLocalState,
} from './state.js';

export interface VaultAbierto {
  runtime: BrowserRuntime;
  sesion: SessionState;
  vault: VaultLocalState;
  subkeys: SubKeys;
  /** Token del device (env primero): lo usa `sync.open`. */
  token: string;
  cerrar(): Promise<void>;
}

const sanitizar = (servidor: string): string =>
  servidor.replace(/^https?:\/\//, '').replace(/[^a-zA-Z0-9._-]/g, '_');

const raizRuntime = (): string =>
  process.env.OVAULTD_RUNTIME_DIR ?? join(homedir(), '.local', 'share', 'ovaultd', 'runtime');

/** Perfil del runtime por vault: un origen estable propio por cada uno. */
export const runtimeDirDe = (server: string, vault: number): string =>
  join(raizRuntime(), `${sanitizar(server)}_${vault}`);

/** Borra el perfil/estado durable local del vault (`unlink --purge`). */
export const purgarRuntimeDir = (server: string, vault: number): void => {
  rmSync(runtimeDirDe(server, vault), { recursive: true, force: true });
};

/**
 * Abre el vault local: resuelve sesión + estado del vault, pide la password, deriva
 * las subclaves y deja el runtime listo (`store.open` + `apply.configure`).
 */
export async function abrirVaultLocal(
  vaultTexto: string | undefined,
  opciones: { passwordStdin?: boolean } = {},
): Promise<VaultAbierto> {
  const sesion = readSession();
  if (!sesion) {
    throw new Error('no hay sesión; corré `ovaultd login <server> --email <email>` primero');
  }

  // Resolución del vault: número explícito, o el único vinculado a una carpeta.
  let vault: VaultLocalState | null = null;
  if (vaultTexto !== undefined && vaultTexto !== '') {
    const numero = Number.parseInt(vaultTexto, 10);
    if (!Number.isInteger(numero) || numero <= 0) throw new Error(`vault inválido: ${vaultTexto}`);
    vault = readVaultState(sesion.server, numero);
    if (!vault) throw new Error(`no hay estado local del vault #${numero} para ${sesion.server}`);
  } else {
    const conDir = listVaultStates().filter((estado) => estado.server === sesion.server && estado.dir);
    if (conDir.length === 0) {
      throw new Error('no hay vaults vinculados a una carpeta; corré `ovaultd setup <vault> --dir <carpeta>`');
    }
    if (conDir.length > 1) {
      throw new Error(`hay ${conDir.length} vaults locales con carpeta; indicá cuál: ovaultd sync <n>`);
    }
    vault = conDir[0]!;
  }

  if (!vault.dir) {
    throw new Error(
      `el vault #${vault.vault} no tiene carpeta vinculada; corré \`ovaultd setup ${vault.vault} --dir <carpeta>\``,
    );
  }

  // Subclaves: si están cacheadas y la epoch no cambió, NO se pide la password (un
  // timer no puede tipearla). Si no, se derivan una vez y se cachean en el estado 0600.
  const salt = b64Decode(vault.salt);
  const vaultUuid = b64Decode(vault.vault_id);
  let subkeys: SubKeys;
  if (vault.subkeys && vault.subkeys_epoch === vault.epoch) {
    subkeys = subclavesDeHex(vault.subkeys);
  } else {
    const password = await resolvePassword({
      passwordStdin: opciones.passwordStdin,
      prompt: `Password del vault #${vault.vault} «${vault.name}»: `,
    });
    const kek = deriveKek(password, salt, vault.kdf_params);
    const vaultKey = unwrapKeyPw(b64Decode(vault.wrapped_key_pw), kek, vaultUuid, salt);
    subkeys = deriveSubkeys(vaultKey, salt);
    saveVaultState({ ...vault, subkeys: subclavesAHex(subkeys), subkeys_epoch: vault.epoch });
    process.stderr.write(
      'claves cacheadas en el estado local (0600); `ovaultd olvidar-clave` para borrarlas\n',
    );
  }

  const runtime = await BrowserRuntime.open({
    dataDir: runtimeDirDe(sesion.server, vault.vault),
    vaultRoot: vault.dir,
  });

  try {
    await runtime.call('store.open', { name: 'cli' });
    await runtime.call('apply.configure', {
      keys: {
        content: toHex(subkeys.content),
        nonce: toHex(subkeys.nonce),
        path: toHex(subkeys.path),
        meta: toHex(subkeys.meta),
        bind: toHex(subkeys.bind),
        sign: toHex(subkeys.sign),
      },
    });
  } catch (error) {
    await runtime.close();
    throw error;
  }

  const token = process.env.OVAULTD_TOKEN ?? sesion.token;

  return {
    runtime,
    sesion,
    vault,
    subkeys,
    token,
    cerrar: () => runtime.close(),
  };
}

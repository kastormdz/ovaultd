#!/usr/bin/env node
/**
 * CLI de ovaultd — fase 1: `login`, `logout`, `list-remote`, `create-remote`,
 * `setup`, `status`.
 *
 * Toda la cripto ocurre acá: el server nunca ve la password, la vault_key ni la
 * recovery. Lo que queda en disco es material PÚBLICO + el token del device.
 */

import { hostname } from 'node:os';
import { chmodSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { Command } from 'commander';

import {
  KDF_DEFAULTS,
  deriveKek,
  deriveKekRecovery,
  deriveSubkeys,
  bytesToUuid,
  identityFromSubkeys,
  randomRecoverySecret,
  randomSalt,
  randomVaultKey,
  randomVaultUuid,
  unwrapKeyPw,
  wrapKeyPw,
  wrapKeyRc,
} from '@ovaultd/crypto';
import {
  b64Decode,
  b64Encode,
  toHex,
  type CreateVaultWire,
  type KdfParamsWire,
} from '@ovaultd/protocol';

import { ApiError, OvaultdClient, normalizeServer } from './api.js';
import { resolvePassword } from './credentials.js';
import {
  clearSession,
  configDir,
  listVaultStates,
  readSession,
  readVaultState,
  saveSession,
  saveVaultState,
  vaultPath,
  type SessionState,
} from './state.js';

const programa = new Command();

programa
  .name('ovaultd')
  .description('CLI de ovaultd (protocolo sync2) — login, vaults y setup local')
  .version('0.1.0');

const exigirSesion = (): SessionState => {
  const sesion = readSession();
  if (!sesion) {
    throw new Error('no hay sesión; corré `ovaultd login <server> --email <email>` primero');
  }
  return sesion;
};

const clienteDe = (sesion: SessionState): OvaultdClient => new OvaultdClient(sesion.server, sesion.token);

const kdfWire = (): KdfParamsWire => ({
  m_kib: KDF_DEFAULTS.m_kib,
  t: KDF_DEFAULTS.t,
  p: KDF_DEFAULTS.p,
  dk_len: KDF_DEFAULTS.dk_len,
});

// ---------------------------------------------------------------------- login

programa
  .command('login <server>')
  .description('inicia sesión (token de device; la password no se guarda)')
  .requiredOption('--email <email>', 'email de la cuenta')
  .option('--password-stdin', 'lee la password de stdin (para scripts)')
  .option('--device-name <name>', 'nombre del device', hostname())
  .option('--platform <platform>', 'plataforma del device', 'cli')
  .action(async (server: string, opciones: { email: string; passwordStdin?: boolean; deviceName: string; platform: string }) => {
    const cliente = new OvaultdClient(server);
    const password = await resolvePassword({ passwordStdin: opciones.passwordStdin });

    const sesion = await cliente.signin(opciones.email, password, opciones.deviceName, opciones.platform);
    cliente.setToken(sesion.token);
    const me = await cliente.me();

    saveSession({
      server: normalizeServer(server),
      email: me.email,
      token: sesion.token,
      device_id: sesion.device_id,
      account_id: me.account_id,
      saved_at: new Date().toISOString(),
    });

    console.log(`sesión iniciada: ${me.email} (cuenta #${me.account_id}, device #${sesion.device_id})`);
    console.log(`estado en ${configDir()}/session.json`);
  });

// --------------------------------------------------------------------- logout

programa
  .command('logout')
  .description('cierra la sesión (revoca el token en el server) y borra el estado local')
  .action(async () => {
    const sesion = readSession();
    if (!sesion) {
      console.log('no hay sesión');
      return;
    }
    try {
      await clienteDe(sesion).signout();
    } catch (error) {
      console.warn(`aviso: signout remoto falló (${String(error)}); borro la sesión local igual`);
    }
    clearSession();
    console.log('sesión cerrada');
  });

// ---------------------------------------------------------------- list-remote

programa
  .command('list-remote')
  .description('lista los vaults del server')
  .action(async () => {
    const sesion = exigirSesion();
    const vaults = await clienteDe(sesion).listVaults();
    if (vaults.length === 0) {
      console.log('sin vaults todavía');
      return;
    }
    for (const vault of vaults) {
      console.log(`#${vault.id}  ${vault.name}  ${vault.role}  rev=${vault.rev}`);
    }
  });

// -------------------------------------------------------------- create-remote

programa
  .command('create-remote <name>')
  .description('crea un vault remoto (genera el material; la recovery se muestra UNA vez)')
  .option('--password-stdin', 'lee la password de stdin (para scripts)')
  .option('--recovery-out <file>', 'escribe la recovery key en un archivo 0600 en vez de imprimirla')
  .action(async (name: string, opciones: { passwordStdin?: boolean; recoveryOut?: string }) => {
    const sesion = exigirSesion();
    const password = await resolvePassword({
      passwordStdin: opciones.passwordStdin,
      prompt: 'Password del vault (protege la clave de datos): ',
    });

    const vaultUuid = randomVaultUuid();
    const vaultKey = randomVaultKey();
    const salt = randomSalt();
    const recoverySecret = randomRecoverySecret();

    const kek = deriveKek(password, salt);
    const wrappedPw = wrapKeyPw(vaultKey, kek, vaultUuid, salt);
    const kekRecovery = deriveKekRecovery(recoverySecret, salt);
    const wrappedRc = wrapKeyRc(vaultKey, kekRecovery, vaultUuid);
    const identidad = identityFromSubkeys(deriveSubkeys(vaultKey, salt));

    const uuidB64 = b64Encode(vaultUuid);
    const saltB64 = b64Encode(salt);
    const wrappedPwB64 = b64Encode(wrappedPw);
    const wrappedRcB64 = b64Encode(wrappedRc);
    const pubkeyB64 = b64Encode(identidad.pubkey);

    const cuerpo: CreateVaultWire = {
      name,
      salt: saltB64,
      kdf_params: kdfWire(),
      wrapped_key_pw: wrappedPwB64,
      wrapped_key_rc: wrappedRcB64,
      vault_pubkey: pubkeyB64,
      vault_id: uuidB64,
    };

    const { id } = await clienteDe(sesion).createVault(cuerpo);

    saveVaultState({
      server: sesion.server,
      vault: id,
      vault_id: uuidB64,
      name,
      role: 'owner',
      epoch: 1,
      salt: saltB64,
      kdf_params: cuerpo.kdf_params,
      wrapped_key_pw: wrappedPwB64,
      wrapped_key_rc: wrappedRcB64,
      vault_pubkey: pubkeyB64,
      saved_at: new Date().toISOString(),
    });

    const recoveryHex = toHex(recoverySecret);
    console.log(`vault creado: #${id} «${name}» (uuid ${bytesToUuid(vaultUuid)})`);
    console.log(`material público guardado en ${vaultPath(sesion.server, id)}`);

    if (opciones.recoveryOut) {
      const destino = resolve(opciones.recoveryOut);
      writeFileSync(destino, `${recoveryHex}\n`, { mode: 0o600 });
      chmodSync(destino, 0o600);
      console.log(`recovery key escrita en ${destino} (0600) — guardala FUERA de este equipo también`);
    } else {
      console.log('');
      console.log('  ⚠️  RECOVERY KEY (no se vuelve a mostrar):');
      console.log(`      ${recoveryHex}`);
      console.log('  Guardala ahora. Es la única forma de recuperar el vault si olvidás la password.');
      console.log('  (Mejor: repetí con --recovery-out <archivo> y guardala en tu gestor de secretos.)');
    }
  });

// ---------------------------------------------------------------------- setup

programa
  .command('setup <vault>')
  .description('vincula una carpeta local a un vault remoto (verifica tu password contra el material)')
  .requiredOption('--dir <dir>', 'carpeta local del vault')
  .option('--password-stdin', 'lee la password de stdin (para scripts)')
  .action(async (vaultTexto: string, opciones: { dir: string; passwordStdin?: boolean }) => {
    const sesion = exigirSesion();
    const vault = Number.parseInt(vaultTexto, 10);
    if (!Number.isInteger(vault) || vault <= 0) throw new Error(`vault inválido: ${vaultTexto}`);

    const cliente = clienteDe(sesion);
    const material = await cliente.material(vault);
    if (!material.vault_id) {
      throw new Error('el vault no tiene vault_id (material incompleto: no se puede derivar la clave)');
    }

    const vaultUuid = b64Decode(material.vault_id);
    const salt = b64Decode(material.salt);
    const params = JSON.parse(material.kdf_params) as KdfParamsWire;

    const password = await resolvePassword({
      passwordStdin: opciones.passwordStdin,
      prompt: 'Password del vault: ',
    });

    const kek = deriveKek(password, salt, params);
    // Si la password no es la correcta, esto tira `wrong_password`. La clave se
    // verifica y se DESCARTA: no se persiste nada secreto.
    unwrapKeyPw(b64Decode(material.wrapped_key_pw), kek, vaultUuid, salt);

    saveVaultState({
      server: sesion.server,
      vault,
      vault_id: material.vault_id,
      name: material.name,
      role: 'owner',
      epoch: material.epoch,
      salt: material.salt,
      kdf_params: params,
      wrapped_key_pw: material.wrapped_key_pw,
      wrapped_key_rc: material.wrapped_key_rc,
      vault_pubkey: material.vault_pubkey,
      dir: resolve(opciones.dir),
      saved_at: new Date().toISOString(),
    });

    console.log(`vault #${vault} «${material.name}» vinculado a ${resolve(opciones.dir)}`);
    console.log('password verificada contra el material del vault ✓ (la clave no se guardó)');
  });

// --------------------------------------------------------------------- status

programa
  .command('status')
  .description('estado de la sesión, del server y de los vaults locales')
  .action(async () => {
    const sesion = readSession();
    if (!sesion) {
      console.log('sin sesión. Corré `ovaultd login <server> --email <email>`');
      return;
    }

    console.log(`server:  ${sesion.server}`);
    console.log(`cuenta:  ${sesion.email} (#${sesion.account_id})`);
    console.log(`device:  #${sesion.device_id}`);
    console.log(`config:  ${configDir()}`);

    const cliente = clienteDe(sesion);
    const sano = await cliente.healthz();
    console.log(`healthz: ${sano ? 'ok' : 'SIN RESPUESTA'}`);

    if (sano) {
      try {
        const vaults = await cliente.listVaults();
        console.log(`vaults remotos: ${vaults.length}`);
        for (const vault of vaults) {
          const local = readVaultState(sesion.server, vault.id);
          const marca = local ? (local.dir ? `→ ${local.dir}` : 'material local, sin dir') : 'sin estado local';
          console.log(`  #${vault.id}  ${vault.name}  rev=${vault.rev}  (${marca})`);
        }
      } catch (error) {
        console.warn(`aviso: no pude listar vaults (${String(error)})`);
      }
    }

    const locales = listVaultStates().filter((estado) => estado.server === sesion.server);
    if (locales.length > 0) {
      console.log(`vaults con estado local: ${locales.length}`);
    }
  });

// ----------------------------------------------------------------------- main

programa.parseAsync(process.argv).catch((error: unknown) => {
  if (error instanceof ApiError) {
    console.error(`error del server: ${error.message}`);
  } else {
    console.error(`error: ${error instanceof Error ? error.message : String(error)}`);
  }
  process.exit(1);
});

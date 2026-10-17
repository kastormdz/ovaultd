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

import { backoffMs } from '@ovaultd/core';
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
import { abrirVaultLocal, purgarRuntimeDir, runtimeDirDe } from './local-runtime.js';
import {
  clearSession,
  configDir,
  listVaultStates,
  readSession,
  readVaultState,
  removeVaultState,
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

// ----------------------------------------------------------------------- sync

const dormir = (ms: number): Promise<void> => new Promise((resolver) => setTimeout(resolver, ms));
const hora = (): string => new Date().toTimeString().slice(0, 8);

programa
  .command('sync [vault]')
  .description('una pasada (o --continuous): sube los cambios locales y baja los remotos')
  .option('--continuous', 'sigue corriendo: intervalos + backoff con reconexión')
  .option('--interval <segundos>', 'segundos entre pasadas en --continuous', '5')
  .option('--password-stdin', 'lee la password de stdin (para scripts)')
  .action(
    async (
      vaultTexto: string | undefined,
      opciones: { continuous?: boolean; interval: string; passwordStdin?: boolean },
    ) => {
      const abierto = await abrirVaultLocal(vaultTexto, opciones);
      try {
        // Recuperación al abrir: resuelve lo que un crash anterior dejó a medias.
        const recuperado = (await abierto.runtime.call('apply.recover')) as {
          completed: number;
          cleaned: number;
          blocked: number;
        };
        if (recuperado.completed + recuperado.cleaned + recuperado.blocked > 0) {
          console.log(
            `recuperación al abrir: ${recuperado.completed} completadas, ${recuperado.cleaned} limpiadas, ${recuperado.blocked} bloqueadas`,
          );
        }

        const conectar = async (): Promise<void> => {
          const checkpoint = (await abierto.runtime.call('store.checkpoint.read')) as {
            since: number;
          } | null;
          await abierto.runtime.call('sync.open', {
            baseUrl: abierto.sesion.server,
            token: abierto.token,
            vault: abierto.vault.vault,
            device: abierto.sesion.device_id,
            since: checkpoint?.since ?? 0,
            vaultUuid: abierto.vault.vault_id,
            epoch: abierto.vault.epoch,
          });
        };

        const pasada = async (): Promise<void> => {
          // Orden del plan (§4.2): PULL primero — la base queda al día y el push que
          // sigue rebasea con el head actual (una operación que quedó pendiente de una
          // corrida anterior se firma/commitea contra la base NUEVA, y si su contenido
          // ya no aplica el server la trata como conflicto real). Lo local vive en disco:
          // el guard de tercera versión impide que el pull lo pise (`blocked`).
          const bajado = (await abierto.runtime.call('sync.pullOnce')) as {
            status: string;
            applied: number;
            blocked?: { rev: number; reason?: string };
            resync?: { since: number; minRev: number };
          };
          const empujado = (await abierto.runtime.call('sync.pushOnce')) as {
            encoladas: number;
            empujadas: number;
            headRev: number;
            conflicto?: { path?: string; mensaje: string };
          };

          console.log(
            `${hora()} ↓ ${bajado.applied} aplicada(s) · ↑ ${empujado.empujadas} subida(s), rev ${empujado.headRev}`,
          );
          if (empujado.conflicto) {
            console.warn(
              `  ⚠ conflicto en ${empujado.conflicto.path ?? '(sin path)'}: ${empujado.conflicto.mensaje}`,
            );
            console.warn('    la operación queda pendiente en la outbox; no se pierde nada');
          }
          if (bajado.status === 'blocked') {
            console.warn(
              `  ⚠ revisión ${bajado.blocked?.rev} bloqueada (${bajado.blocked?.reason ?? 'tercera versión local'}); el checkpoint NO avanza`,
            );
          }
          if (bajado.status === 'resync-required') {
            console.warn(
              `  ⚠ resync requerido: since=${bajado.resync?.since} quedó bajo min_rev=${bajado.resync?.minRev} (el estado local NO se borra)`,
            );
          }
        };

        await conectar();
        if (!opciones.continuous) {
          await pasada();
          return;
        }

        const intervaloSeg = Math.max(1, Number.parseInt(opciones.interval, 10) || 5);
        let parar = false;
        process.once('SIGINT', () => {
          parar = true;
          console.log('\nSIGINT: terminando la pasada y saliendo…');
        });
        console.log(`modo continuo: cada ~${intervaloSeg} s (Ctrl-C para salir)`);

        let fallas = 0;
        while (!parar) {
          try {
            await pasada();
            fallas = 0;
          } catch (error) {
            fallas = Math.min(fallas + 1, 7);
            const espera = backoffMs(fallas - 1);
            console.warn(
              `pasada falló: ${error instanceof Error ? error.message : String(error)}; reconecto en ${Math.round(espera / 1000)} s`,
            );
            try {
              await abierto.runtime.call('sync.close');
            } catch {
              /* ya estaba cerrada */
            }
            await dormir(espera);
            await conectar();
            continue;
          }
          await dormir(intervaloSeg * 1000 + Math.floor(Math.random() * 1000));
        }
        await abierto.runtime.call('sync.close');
      } finally {
        await abierto.cerrar();
      }
    },
  );

// ---------------------------------------------------------------- list-local

programa
  .command('list-local [vault]')
  .description('estado local del vault: escaneo + hash-diff (sin tocar la red)')
  .option('--password-stdin', 'lee la password de stdin (para scripts)')
  .action(async (vaultTexto: string | undefined, opciones: { passwordStdin?: boolean }) => {
    const abierto = await abrirVaultLocal(vaultTexto, opciones);
    try {
      const { diff, entradas } = (await abierto.runtime.call('sync.scan', {})) as {
        diff: {
          nuevos: Array<{ pathId: string }>;
          modificados: Array<{ pathId: string }>;
          borrados: Array<{ pathId: string; path?: string }>;
          sinCambios: number;
        };
        entradas: Array<{ path: string; pathId: string; size: number }>;
      };
      const pendientes = (await abierto.runtime.call('outbox.list')) as unknown[];

      const estado = new Map<string, string>();
      for (const entrada of diff.nuevos) estado.set(entrada.pathId, 'nuevo');
      for (const entrada of diff.modificados) estado.set(entrada.pathId, 'modificado');
      for (const entrada of diff.borrados) estado.set(entrada.pathId, 'borrado');

      console.log(`vault #${abierto.vault.vault} «${abierto.vault.name}» → ${abierto.vault.dir}`);
      console.log(
        `${entradas.length} archivo(s) · nuevos=${diff.nuevos.length} modificados=${diff.modificados.length} ` +
          `borrados=${diff.borrados.length} sinCambios=${diff.sinCambios} pendientesEnOutbox=${pendientes.length}`,
      );
      for (const entrada of entradas) {
        console.log(`  [${estado.get(entrada.pathId) ?? 'igual'}] ${entrada.path} (${entrada.size} B)`);
      }
      for (const borrado of diff.borrados) {
        console.log(`  [borrado] ${borrado.path ?? borrado.pathId}`);
      }
    } finally {
      await abierto.cerrar();
    }
  });

// --------------------------------------------------------------------- config

programa
  .command('config')
  .description('muestra la configuración local (sin secretos)')
  .action(() => {
    const sesion = readSession();
    console.log(`config dir: ${configDir()}`);
    if (!sesion) {
      console.log('sin sesión (ovaultd login <server> --email <email>)');
    } else {
      console.log(`server:  ${sesion.server}`);
      console.log(`cuenta:  ${sesion.email} (#${sesion.account_id})`);
      console.log(`device:  #${sesion.device_id}`);
      console.log(
        `token:   ${process.env.OVAULTD_TOKEN ? 'por OVAULTD_TOKEN (env; no se persiste aparte)' : 'persistido (0600) en session.json'}`,
      );
    }

    const locales = listVaultStates();
    if (locales.length === 0) {
      console.log('vaults locales: ninguno');
      return;
    }
    console.log(`vaults locales: ${locales.length}`);
    for (const vault of locales) {
      const dir = vault.dir ? `→ ${vault.dir}` : '(sin carpeta)';
      console.log(`  #${vault.vault} «${vault.name}» ${dir}  server=${vault.server}`);
      console.log(`    perfil del runtime: ${runtimeDirDe(vault.server, vault.vault)}`);
    }
  });

// --------------------------------------------------------------------- unlink

programa
  .command('unlink <vault>')
  .description('desvincula la carpeta local (borra el estado local; el server no se toca)')
  .option('--purge', 'borra además el perfil del runtime (IndexedDB/outbox local)')
  .action((vaultTexto: string, opciones: { purge?: boolean }) => {
    const sesion = exigirSesion();
    const vault = Number.parseInt(vaultTexto, 10);
    if (!Number.isInteger(vault) || vault <= 0) throw new Error(`vault inválido: ${vaultTexto}`);

    const estado = readVaultState(sesion.server, vault);
    if (!estado) throw new Error(`no hay estado local del vault #${vault} para ${sesion.server}`);

    removeVaultState(sesion.server, vault);
    console.log(`vault #${vault} «${estado.name}» desvinculado${estado.dir ? ` de ${estado.dir}` : ''}`);
    if (opciones.purge) {
      purgarRuntimeDir(sesion.server, vault);
      console.log('perfil del runtime borrado (--purge): la outbox local y el checkpoint se descartaron');
    } else {
      console.log(`el perfil del runtime quedó en ${runtimeDirDe(sesion.server, vault)} (--purge lo borra)`);
    }
    console.log('el vault remoto sigue intacto; nada se borró del server');
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

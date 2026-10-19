/**
 * Comandos de filesystem del vault (`cat`/`write`/`ls`/`rm`): la base del agente que
 * trabaja sobre el vault sincronizado. Lo que se prueba acá es el CONFINAMIENTO (una
 * ruta no puede escapar del vault ni por travesía ni por symlink) y el CRUD real.
 */
import { mkdtempSync, mkdirSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { saveVaultState } from '../src/state.js';
import {
  borrarDelVault,
  escribirEnVault,
  leerDelVault,
  listarVault,
  resolverVaultLocal,
  rutaDelVault,
} from '../src/vault-fs.js';

let raiz: string;
let config: string;

beforeEach(() => {
  raiz = mkdtempSync(join(tmpdir(), 'ovaultd-vaultfs-'));
  config = mkdtempSync(join(tmpdir(), 'ovaultd-config-'));
  process.env.OVAULTD_CONFIG_DIR = config;
  mkdirSync(join(raiz, 'Notas'), { recursive: true });
  mkdirSync(join(raiz, '.obsidian'), { recursive: true });
  writeFileSync(join(raiz, '.obsidian', 'app.json'), '{}');
});

afterEach(() => {
  delete process.env.OVAULTD_CONFIG_DIR;
  rmSync(raiz, { recursive: true, force: true });
  rmSync(config, { recursive: true, force: true });
});

describe('confinamiento de rutas', () => {
  it('rechaza travesía, rutas absolutas y segmentos vacíos', () => {
    expect(() => rutaDelVault(raiz, '../fuera.md')).toThrow(/travesía|inválida/);
    expect(() => rutaDelVault(raiz, '/etc/passwd')).toThrow(/absoluta|inválida/);
    expect(() => rutaDelVault(raiz, 'Notas//doble.md')).toThrow(/inválida/);
    expect(() => rutaDelVault(raiz, '')).toThrow(/vacía/);
  });

  it('un symlink que apunta fuera del vault no deja escapar', () => {
    symlinkSync('/etc', join(raiz, 'afuera'));
    expect(() => rutaDelVault(raiz, 'afuera/passwd')).toThrow(/symlink|fuera del vault/);
  });

  it('acepta archivos y subcarpetas dentro del vault', () => {
    // El TMPDIR de Hermes es un symlink: la ruta devuelta es la REAL (más segura).
    expect(rutaDelVault(raiz, 'Notas/uno.md')).toBe(join(realpathSync(raiz), 'Notas/uno.md'));
  });
});

describe('CRUD del vault local', () => {
  it('escribe (creando carpetas), lee y borra', () => {
    const escritos = escribirEnVault(raiz, 'Notas/nueva.md', new TextEncoder().encode('# Hola\n'));
    expect(escritos).toBe(7);
    expect(new TextDecoder().decode(leerDelVault(raiz, 'Notas/nueva.md'))).toBe('# Hola\n');

    borrarDelVault(raiz, 'Notas/nueva.md', false);
    expect(() => leerDelVault(raiz, 'Notas/nueva.md')).toThrow();
  });

  it('la escritura es atómica: no deja temporales', () => {
    escribirEnVault(raiz, 'Notas/atomica.md', new TextEncoder().encode('x'));
    const listado = listarVault(raiz, 'Notas');
    expect(listado.map((e) => e.path)).toEqual(['Notas/atomica.md']);
  });

  it('lista sin .obsidian por defecto y con --todo lo incluye', () => {
    escribirEnVault(raiz, 'Notas/a.md', new TextEncoder().encode('a'));
    escribirEnVault(raiz, 'b.md', new TextEncoder().encode('bb'));

    const normal = listarVault(raiz).map((e) => e.path).sort();
    expect(normal).toEqual(['Notas/a.md', 'b.md'].sort());

    const todo = listarVault(raiz, '', true).map((e) => e.path);
    expect(todo).toContain('.obsidian/app.json');
  });

  it('borra un directorio solo con recursivo', () => {
    escribirEnVault(raiz, 'Notas/a.md', new TextEncoder().encode('a'));
    escribirEnVault(raiz, 'Notas/sub/x.md', new TextEncoder().encode('x'));
    expect(() => borrarDelVault(raiz, 'Notas/sub', false)).toThrow();
    borrarDelVault(raiz, 'Notas/sub', true);
    expect(readFileSync(join(raiz, 'Notas', 'a.md'), 'utf8')).toBe('a'); // el resto intacto
  });
});

describe('resolución del vault', () => {
  it('con un solo vault vinculado lo devuelve; sin vaults, error claro', () => {
    expect(() => resolverVaultLocal()).toThrow(/ningún vault vinculado/);

    saveVaultState({
      server: 'http://127.0.0.1:9999',
      vault: 7,
      vault_id: 'AAAAAAAAAAAAAAAAAAAAAA==',
      name: 'prueba',
      role: 'owner',
      epoch: 1,
      salt: 'AA==',
      kdf_params: { algo: 'argon2id', m: 1, t: 1, p: 1 },
      wrapped_key_pw: 'AA==',
      wrapped_key_rc: 'AA==',
      vault_pubkey: 'AA==',
      dir: raiz,
      saved_at: new Date().toISOString(),
    });

    const vault = resolverVaultLocal();
    expect(vault.vault).toBe(7);
    expect(vault.dir).toBe(raiz);
  });
});

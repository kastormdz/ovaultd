/**
 * Acceso al vault LOCAL por filesystem — la base del "dispositivo-agente" (fase 4):
 * con el vault sincronizado en disco, un agente hace CRUD sobre archivos y `sync`
 * propaga los cambios. Nada de esto toca la red ni el server (E2EE: el server no ve
 * texto plano).
 *
 * Todo path relativo pasa por `validarRutaDeVault` (mismas reglas que el protocolo) y
 * por un chequeo de symlinks: una ruta nunca puede escapar de la carpeta del vault.
 */
import { mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve, sep } from 'node:path';

import { validarRutaDeVault } from '@ovaultd/core';

import { listVaultStates, type VaultLocalState } from './state.js';

/** Resuelve el vault local desde el estado guardado, sin pedir password ni abrir el runtime. */
export function resolverVaultLocal(vaultTexto?: string): VaultLocalState {
  const vaults = listVaultStates().filter((vault) => vault.dir !== undefined);
  if (vaults.length === 0) {
    throw new Error('no hay ningún vault vinculado a una carpeta (corré `ovaultd setup`)');
  }
  if (vaultTexto !== undefined && vaultTexto !== '') {
    const numero = Number(vaultTexto);
    const encontrado = vaults.find((vault) => vault.vault === numero);
    if (!encontrado) throw new Error(`el vault #${vaultTexto} no está vinculado a una carpeta`);
    return encontrado;
  }
  if (vaults.length === 1) return vaults[0]!;
  throw new Error(
    `hay ${vaults.length} vaults vinculados; indicá cuál con --vault <n> (${vaults
      .map((vault) => `#${vault.vault} «${vault.name}»`)
      .join(', ')})`,
  );
}

/** Ruta absoluta de `rel` dentro del vault, o error si escapa (travesía, absoluta, symlink). */
export function rutaDelVault(raiz: string, rel: string): string {
  const validacion = validarRutaDeVault(rel);
  if (!validacion.ok) throw new Error(`ruta inválida: ${validacion.motivo}`);
  if (rel === '') throw new Error('ruta vacía');

  const raizReal = realpathSync(raiz);
  const absoluta = resolve(raizReal, rel);
  if (absoluta !== raizReal && !absoluta.startsWith(raizReal + sep)) {
    throw new Error('la ruta apunta fuera del vault');
  }

  // El directorio padre no puede ser un symlink que escape (si todavía no existe, se crea).
  try {
    const padre = realpathSync(dirname(absoluta));
    if (padre !== raizReal && !padre.startsWith(raizReal + sep)) {
      throw new Error('la ruta escapa del vault por un symlink');
    }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
  }

  return absoluta;
}

/** Escribe `bytes` en una ruta del vault: crea carpetas y publica con rename atómico. */
export function escribirEnVault(raiz: string, rel: string, bytes: Uint8Array): number {
  const absoluta = rutaDelVault(raiz, rel);
  mkdirSync(dirname(absoluta), { recursive: true });
  const temporal = `${absoluta}.tmp-${process.pid}`;
  writeFileSync(temporal, bytes);
  renameSync(temporal, absoluta);
  return bytes.byteLength;
}

/** Lee una ruta del vault (binario-safe). */
export function leerDelVault(raiz: string, rel: string): Uint8Array {
  return readFileSync(rutaDelVault(raiz, rel));
}

export interface EntradaDelVault {
  path: string;
  size: number;
  mtimeMs: number;
}

/**
 * Lista archivos del vault (relativo, sin seguir symlinks). `.obsidian/` queda afuera
 * salvo `incluirConfig`: es ruido para un agente y cambia solo con la app.
 */
export function listarVault(raiz: string, prefijo = '', incluirConfig = false): EntradaDelVault[] {
  const base = prefijo === '' ? raiz : rutaDelVault(raiz, prefijo);
  const salida: EntradaDelVault[] = [];

  const caminar = (dir: string, relDir: string): void => {
    for (const entrada of readdirSync(dir, { withFileTypes: true })) {
      const rel = relDir === '' ? entrada.name : `${relDir}/${entrada.name}`;
      if (!incluirConfig && (rel === '.obsidian' || rel.startsWith('.obsidian/'))) continue;
      if (entrada.isSymbolicLink()) continue; // no se siguen: podrían escapar del vault
      if (entrada.isDirectory()) {
        caminar(join(dir, entrada.name), rel);
        continue;
      }
      if (!entrada.isFile()) continue;
      const info = statSync(join(dir, entrada.name));
      salida.push({ path: rel, size: info.size, mtimeMs: info.mtimeMs });
    }
  };

  caminar(base, prefijo);
  return salida.sort((a, b) => a.path.localeCompare(b.path));
}

/** Borra un archivo (o un directorio con `recursivo`). Devuelve qué borró. */
export function borrarDelVault(raiz: string, rel: string, recursivo: boolean): void {
  const absoluta = rutaDelVault(raiz, rel);
  rmSync(absoluta, { recursive: recursivo, force: false });
}

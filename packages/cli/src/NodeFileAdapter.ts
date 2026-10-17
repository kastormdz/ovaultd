/**
 * NodeFileAdapter — el puente de archivos del CLI hacia un vault.
 *
 * Reglas (plan de fase 2, paso 1):
 *  - Toda operación queda **confinada a la raíz del vault**: rutas relativas, sin
 *    travesía (`..`) y el resultado absoluto tiene que caer dentro.
 *  - **No sigue symlinks**: si el camino físico (realpath) difiere del textual, la
 *    operación se rechaza en vez de escapar del vault.
 *  - **No expone nada a la red**: lo consume el servidor local del runtime
 *    (127.0.0.1), que solo habla con la página del runtime.
 *
 * Las operaciones son las mínimas del motor de aplicación: `stat`, `read`, escribir un
 * temporal en el directorio destino (con `fsync`: tiene que sobrevivir a un kill), `rename`
 * atómico, `unlink`, `mkdirp` y `list`.
 */

import { randomBytes } from 'node:crypto';
import {
  closeSync,
  fsyncSync,
  lstatSync,
  mkdirSync,
  openSync,
  readdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  statSync,
  unlinkSync,
  writeSync,
} from 'node:fs';
import { dirname, isAbsolute, join, normalize, resolve, sep } from 'node:path';

export class VaultBoundaryError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'VaultBoundaryError';
  }
}

export interface VaultStat {
  size: number;
  mtimeMs: number;
  isFile: boolean;
  isDirectory: boolean;
}

export interface VaultDirEntry {
  name: string;
  isFile: boolean;
  isDirectory: boolean;
}

const TEMP_PREFIX = '.ovaultd-tmp-';

export class NodeFileAdapter {
  readonly root: string;

  constructor(root: string) {
    // La raíz se canonicaliza UNA vez: a partir de acá, todo realpath tiene que coincidir.
    this.root = realpathSync(resolve(root));
  }

  /** Valida `rel` (relativa, sin travesía) y devuelve la ruta absoluta textual. */
  private absoluta(rel: string): string {
    if (typeof rel !== 'string' || rel.length === 0 || rel.includes('\0')) {
      throw new VaultBoundaryError(`ruta inválida: ${String(rel)}`);
    }
    if (isAbsolute(rel)) throw new VaultBoundaryError(`ruta absoluta rechazada: ${rel}`);
    const norm = normalize(rel);
    if (norm === '..' || norm.startsWith(`..${sep}`)) {
      throw new VaultBoundaryError(`travesía rechazada: ${rel}`);
    }
    const abs = resolve(this.root, norm);
    if (abs !== this.root && !abs.startsWith(this.root + sep)) {
      throw new VaultBoundaryError(`fuera del vault: ${rel}`);
    }
    return abs;
  }

  /** Rechaza si algún componente existente del camino pasa por un symlink. */
  private canonical(abs: string): void {
    let actual = abs;
    for (;;) {
      let existe = true;
      try {
        lstatSync(actual);
      } catch {
        existe = false;
      }
      if (existe) {
        const real = realpathSync(actual);
        if (real !== actual) {
          throw new VaultBoundaryError(`symlink en el camino: ${actual} -> ${real}`);
        }
        return;
      }
      const padre = dirname(actual);
      if (padre === actual) return;
      actual = padre;
    }
  }

  stat(rel: string): VaultStat | null {
    const abs = this.absoluta(rel);
    try {
      lstatSync(abs);
    } catch {
      return null;
    }
    this.canonical(abs);
    const st = statSync(abs);
    return {
      size: st.size,
      mtimeMs: st.mtimeMs,
      isFile: st.isFile(),
      isDirectory: st.isDirectory(),
    };
  }

  read(rel: string): Uint8Array {
    const abs = this.absoluta(rel);
    this.canonical(abs);
    return new Uint8Array(readFileSync(abs));
  }

  /** Escribe un temporal en `relDir` (debe existir y ser un directorio real). */
  writeTemp(relDir: string, bytes: Uint8Array): { tempName: string } {
    const dirAbs = this.absoluta(relDir);
    this.canonical(dirAbs);
    const st = statSync(dirAbs);
    if (!st.isDirectory()) throw new VaultBoundaryError(`writeTemp: no es un directorio: ${relDir}`);
    const nombre = `${TEMP_PREFIX}${randomBytes(8).toString('hex')}`;
    const absTmp = join(dirAbs, nombre);
    const fd = openSync(absTmp, 'wx', 0o644);
    try {
      writeSync(fd, bytes);
      fsyncSync(fd); // durabilidad real: el temporal sobrevive a un kill del proceso
    } finally {
      closeSync(fd);
    }
    return { tempName: normalize(join(relDir, nombre)) };
  }

  rename(relFrom: string, relTo: string): void {
    const absFrom = this.absoluta(relFrom);
    const absTo = this.absoluta(relTo);
    this.canonical(absFrom);
    this.canonical(absTo);
    const stFrom = lstatSync(absFrom);
    if (!stFrom.isFile()) throw new VaultBoundaryError(`rename: el origen no es un archivo: ${relFrom}`);
    try {
      const stTo = lstatSync(absTo);
      if (stTo.isDirectory()) throw new VaultBoundaryError(`rename: el destino es un directorio: ${relTo}`);
    } catch (error) {
      if (error instanceof VaultBoundaryError) throw error;
    }
    renameSync(absFrom, absTo);
  }

  unlink(rel: string): void {
    const abs = this.absoluta(rel);
    this.canonical(abs);
    unlinkSync(abs);
  }

  mkdirp(rel: string): void {
    const abs = this.absoluta(rel);
    this.canonical(abs);
    mkdirSync(abs, { recursive: true });
  }

  list(rel: string): VaultDirEntry[] {
    const abs = this.absoluta(rel);
    this.canonical(abs);
    return readdirSync(abs, { withFileTypes: true }).map((entrada) => ({
      name: entrada.name,
      isFile: entrada.isFile(),
      isDirectory: entrada.isDirectory(),
    }));
  }
}

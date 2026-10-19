/**
 * Puerto de filesystem del núcleo: el runtime NO toca archivos directamente — le pide
 * operaciones a un puente que las ejecuta confinadas al vault. En el CLI, ese puente es
 * el `NodeFileAdapter` detrás del servidor local (127.0.0.1); en el plugin de Obsidian
 * será la API del vault. Las rutas son SIEMPRE relativas al vault: el que impone el
 * límite es el puente, no el llamador.
 */

import { b64Decode, b64Encode } from '@ovaultd/protocol';

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

export interface FileSystemPort {
  stat(relPath: string): Promise<VaultStat | null>;
  /**
   * Stat de VARIOS paths en UN round-trip. El escaneo lo usa para no pagar un request
   * por archivo contra el puente (un vault de 1000 notas = 1000 POST: el cuello del T3
   * del bench). Opcional: sin soporte, el escaneo bate el bucle de `stat`.
   */
  statMany?(relPaths: string[]): Promise<Array<VaultStat | null>>;
  read(relPath: string): Promise<Uint8Array>;
  /** Escribe un temporal en `relDir`; devuelve su nombre (relativo al vault). */
  writeTemp(relDir: string, bytes: Uint8Array): Promise<{ tempName: string }>;
  rename(fromRel: string, toRel: string): Promise<void>;
  unlink(relPath: string): Promise<void>;
  mkdirp(relPath: string): Promise<void>;
  list(relDir: string): Promise<VaultDirEntry[]>;
}

/** Implementación sobre el puente HTTP del runtime (mismo origen que sirve la página). */
export class HttpFileSystemPort implements FileSystemPort {
  constructor(private readonly baseUrl: string) {}

  private async op<T>(nombre: string, params: Record<string, unknown>): Promise<T> {
    const respuesta = await fetch(`${this.baseUrl}/fs/${nombre}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(params),
    });
    const datos = (await respuesta.json()) as {
      ok: boolean;
      result?: T;
      error?: { message?: string; name?: string };
    };
    if (!datos.ok) throw new Error(`fs ${nombre}: ${datos.error?.message ?? 'error desconocido'}`);
    return datos.result as T;
  }

  stat(relPath: string): Promise<VaultStat | null> {
    return this.op('stat', { path: relPath });
  }

  async statMany(relPaths: string[]): Promise<Array<VaultStat | null>> {
    const result = await this.op<{ stats: Array<VaultStat | null> }>('statMany', {
      paths: relPaths,
    });
    return result.stats;
  }

  async read(relPath: string): Promise<Uint8Array> {
    const result = await this.op<{ b64: string }>('read', { path: relPath });
    return b64Decode(result.b64);
  }

  writeTemp(relDir: string, bytes: Uint8Array): Promise<{ tempName: string }> {
    return this.op('writeTemp', { dir: relDir, b64: b64Encode(bytes) });
  }

  async rename(fromRel: string, toRel: string): Promise<void> {
    await this.op('rename', { from: fromRel, to: toRel });
  }

  async unlink(relPath: string): Promise<void> {
    await this.op('unlink', { path: relPath });
  }

  async mkdirp(relPath: string): Promise<void> {
    await this.op('mkdirp', { path: relPath });
  }

  list(relDir: string): Promise<VaultDirEntry[]> {
    return this.op('list', { path: relDir });
  }
}

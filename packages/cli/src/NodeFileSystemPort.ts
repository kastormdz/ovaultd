/**
 * `NodeFileSystemPort` — el puerto de archivos del núcleo, ejecutado en el proceso del CLI.
 *
 * El núcleo nunca toca archivos directamente: le pide operaciones a un puerto confinado al
 * vault. Con navegador ese puerto era un bridge HTTP en 127.0.0.1; acá es una llamada
 * directa al `NodeFileAdapter`, que ya impone el confín (rechaza `..`, symlinks que salen
 * del vault y rutas absolutas).
 *
 * Los métodos del adapter son sincrónicos y los del puerto asincrónicos: se envuelven sin
 * agregar concurrencia. El valor está en el confín, no en el paralelismo — en el mismo
 * proceso el costo por archivo es despreciable.
 */

import type { FileSystemPort, VaultDirEntry, VaultStat } from '@ovaultd/core';

import { NodeFileAdapter } from './NodeFileAdapter.js';

export class NodeFileSystemPort implements FileSystemPort {
  constructor(private readonly adapter: NodeFileAdapter) {}

  async stat(relPath: string): Promise<VaultStat | null> {
    return this.adapter.stat(relPath);
  }

  async statMany(relPaths: string[]): Promise<Array<VaultStat | null>> {
    return relPaths.map((rel) => this.adapter.stat(rel));
  }

  async read(relPath: string): Promise<Uint8Array> {
    return this.adapter.read(relPath);
  }

  async readMany(relPaths: string[]): Promise<Array<Uint8Array | null>> {
    return relPaths.map((rel) => {
      try {
        return this.adapter.read(rel);
      } catch {
        // Un archivo que desapareció entre el escaneo y la lectura es "no está", no un
        // error del lote: el núcleo decide qué hacer con el hueco.
        return null;
      }
    });
  }

  async writeTemp(relDir: string, bytes: Uint8Array): Promise<{ tempName: string }> {
    return this.adapter.writeTemp(relDir, bytes);
  }

  async rename(fromRel: string, toRel: string): Promise<void> {
    this.adapter.rename(fromRel, toRel);
  }

  async unlink(relPath: string): Promise<void> {
    this.adapter.unlink(relPath);
  }

  async mkdirp(relPath: string): Promise<void> {
    this.adapter.mkdirp(relPath);
  }

  async list(relDir: string): Promise<VaultDirEntry[]> {
    return this.adapter.list(relDir);
  }
}

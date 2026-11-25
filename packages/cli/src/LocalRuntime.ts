/**
 * `LocalRuntime` — el motor de sync corriendo en el proceso del CLI, sin navegador.
 *
 * Es la contraparte de `BrowserRuntime`: misma superficie (`call` + `close`) y las mismas
 * respuestas, porque las dos llaman al MISMO `dispatch` del núcleo. Lo único que cambia es
 * dónde vive el estado durable (SQLite en vez de IndexedDB) y cómo se llega al filesystem
 * (llamada directa en vez de un bridge HTTP en 127.0.0.1).
 *
 * Consecuencia buscada: no queda ningún puerto local escuchando. El bridge era el que
 * exponía `/fs/*` sin autenticación y el proxy sin allowlist.
 */

import { mkdirSync } from 'node:fs';
import { dirname } from 'node:path';

import { configurarFetchDirecto, configurarFilePort, configurarStore, dispatch } from '@ovaultd/core';

import { NodeFileAdapter } from './NodeFileAdapter.js';
import { NodeFileSystemPort } from './NodeFileSystemPort.js';
import { SqliteStore } from './SqliteStore.js';

export interface LocalRuntimeOptions {
  /** Raíz del vault vinculado: el confín que aplica el adapter. */
  root: string;
  /** Archivo SQLite del estado durable de ese vault. */
  rutaDb: string;
}

export class LocalRuntime {
  private constructor() {}

  /**
   * Apunta el núcleo al estado y al filesystem nativos. Se hace ANTES de `store.open`:
   * el motor abre el estado durable en la primera llamada y usaría IndexedDB.
   */
  static async open(opciones: LocalRuntimeOptions): Promise<LocalRuntime> {
    mkdirSync(dirname(opciones.rutaDb), { recursive: true });

    const port = new NodeFileSystemPort(new NodeFileAdapter(opciones.root));
    configurarFilePort(() => port);
    // Sin origen no hay CORS: el server se alcanza directo, sin el proxy del bridge.
    configurarFetchDirecto(true);
    configurarStore(async () => SqliteStore.open(opciones.rutaDb));

    return new LocalRuntime();
  }

  /** Misma firma y semántica que `BrowserRuntime.call`. */
  async call<T = unknown>(method: string, params?: Record<string, unknown>): Promise<T> {
    const respuesta = await dispatch({ id: 1, method, params });
    if (!respuesta.ok) {
      throw new Error(`runtime ${method}: ${respuesta.error?.message ?? 'error desconocido'}`);
    }
    return respuesta.result as T;
  }

  /** El store lo cierra `store.close`, que el motor ejecuta por su cuenta. */
  async close(): Promise<void> {}
}

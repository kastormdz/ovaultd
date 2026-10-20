/**
 * Tests del prune (reconciliación): colapso del oplog al estado vivo por path,
 * descifrado real de las rutas y comparación contra el disco.
 *
 * El server no puede hacer esto (E2EE): si un borrado local nunca pasó por el
 * sync, el path queda vivo en `files` y el panel lo cuenta como nota. El prune
 * es el único que puede decírselo, porque solo el cliente tiene las claves.
 */

import { describe, expect, it } from 'vitest';

import { deriveSubkeys, pathEnc, pathId, randomSalt, randomVaultKey } from '@ovaultd/crypto';
import { b64Encode, type SignedRecordWire } from '@ovaultd/protocol';

import type { FileSystemPort, VaultStat } from '../src/file-port.js';
import { planificarPrune } from '../src/prune.js';
import type { SyncConnection } from '../src/SyncConnection.js';

const CLAVES = deriveSubkeys(randomVaultKey(), randomSalt());
const BLOB = b64Encode(new Uint8Array(32).fill(7));
const BLOB_CARPETA = b64Encode(new Uint8Array(32));

function registro(op: 'add' | 'mod' | 'del', ruta: string, rev: number): SignedRecordWire {
  return {
    rev,
    op,
    path_id: b64Encode(pathId(CLAVES, ruta)),
    blob_id: op === 'del' ? BLOB : BLOB,
    path_enc: b64Encode(pathEnc(CLAVES, ruta)),
    t_mac: '',
    sig: '',
    size: 0,
    mtime: 0,
    expected_rev: 0,
    operation_id: '',
    epoch: 1,
  };
}

function carpeta(ruta: string, rev: number): SignedRecordWire {
  return { ...registro('add', ruta, rev), blob_id: BLOB_CARPETA };
}

/** Conexión fake: una página y después vacío (sin red). */
function connCon(items: SignedRecordWire[]): SyncConnection {
  let entregada = false;
  return {
    async pull(since: number) {
      if (entregada) return { items: [], next_since: since, min_rev: null };
      entregada = true;
      const next = items.length > 0 ? items[items.length - 1]!.rev : since;
      return { items, next_since: next, min_rev: null };
    },
  } as unknown as SyncConnection;
}

/** Puerto fake: solo `stat` (el prune no lee contenido). */
function portCon(existentes: string[]): FileSystemPort {
  const set = new Set(existentes);
  const stat = async (rel: string): Promise<VaultStat | null> =>
    set.has(rel) ? { size: 1, mtimeMs: 1, isFile: true, isDirectory: false } : null;
  return { stat } as unknown as FileSystemPort;
}

const deps = (items: SignedRecordWire[], existentes: string[]) => ({
  conn: connCon(items),
  keys: CLAVES,
  port: portCon(existentes),
});

describe('prune', () => {
  it('los `del` sacan el path del estado vivo', async () => {
    const plan = await planificarPrune(
      deps([registro('add', 'a.md', 1), registro('add', 'b.md', 2), registro('del', 'a.md', 3)], ['b.md']),
    );
    expect(plan.vivos).toBe(1);
    expect(plan.enDisco).toBe(1);
    expect(plan.huerfanos).toEqual([]);
  });

  it('un path vivo que no existe en disco es huérfano', async () => {
    const plan = await planificarPrune(
      deps([registro('add', 'a.md', 1), registro('add', 'b.md', 2)], ['b.md']),
    );
    expect(plan.vivos).toBe(2);
    expect(plan.enDisco).toBe(1);
    expect(plan.huerfanos).toEqual(['a.md']);
  });

  it('una carpeta (blob_id cero) no cuenta como archivo', async () => {
    const plan = await planificarPrune(deps([carpeta('vacia/', 1)], []));
    expect(plan.vivos).toBe(0);
    expect(plan.huerfanos).toEqual([]);
  });

  it('una ruta hostil se bloquea y nunca se propone borrar', async () => {
    const plan = await planificarPrune(deps([registro('add', '../fuera.md', 1)], []));
    expect(plan.bloqueadas).toBe(1);
    expect(plan.huerfanos).toEqual([]);
  });

  it('los huérfanos salen ordenados y con su ruta descifrada', async () => {
    const plan = await planificarPrune(
      deps([registro('add', 'z.md', 1), registro('add', 'a.md', 2), registro('add', 'm.md', 3)], ['m.md']),
    );
    expect(plan.huerfanos).toEqual(['a.md', 'z.md']);
  });
});

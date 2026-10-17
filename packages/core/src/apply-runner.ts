/**
 * Orquestador de aplicación: intención → temporal verificado → publicar → commit atómico.
 *
 * Invariantes (plan de fase 2, paso 3):
 *  - La intención se persiste ANTES de escribir el temporal, y el temporal se verifica
 *    (sha256 del plaintext) ANTES de publicar.
 *  - El commit es atómico (StateStore.completeApply): `since` avanza SOLO cuando la
 *    revisión quedó recuperablemente aplicada.
 *  - Una **tercera versión local** (archivo modificado después de la base) jamás se pisa:
 *    la intención queda `blocked`, el archivo local intacto, y la resolución es explícita.
 *  - `recoverApplies()` decide desde las INTENCIONES + el estado real del filesystem:
 *    no adivina — compara hashes.
 */

import { cryptoProvider, decryptBlob, type SubKeys } from '@ovaultd/crypto';
import { toHex } from '@ovaultd/protocol';

import { createIntent } from './apply.js';
import type { FileSystemPort } from './file-port.js';
import type { IntentEntry, StateStore } from './StateStore.js';

export interface ApplyRevision {
  /** `path_id` en hex. */
  pathId: string;
  /** Ruta relativa en el vault (texto en claro). */
  path: string;
  kind: 'file' | 'folder';
  deleted: boolean;
  rev: number;
  /** `blob_id` en hex del contenido (null en tombstones/carpetas). */
  blobId: string | null;
  /** sha256 hex del PLAINTEXT esperado (del registro). */
  contentHash?: string | null;
}

export interface ApplyDeps {
  store: StateStore;
  port: FileSystemPort;
  /** Subclaves `sync2` del vault (incluye la de contenido). */
  keys: SubKeys;
}

export type ApplyStatus = 'applied' | 'blocked' | 'noop';

export interface ApplyOutcome {
  status: ApplyStatus;
  intentId: string;
  reason?: string;
}

export interface RecoveryReport {
  completed: number;
  cleaned: number;
  blocked: number;
}

const sha256Hex = (bytes: Uint8Array): string => toHex(cryptoProvider().sha256(bytes));

const dirVault = (path: string): string => {
  const corte = path.lastIndexOf('/');
  return corte < 0 ? '.' : path.slice(0, corte);
};

type Guard =
  | { verdict: 'safe' }
  | { verdict: 'identical' }
  | { verdict: 'already-gone' }
  | { verdict: 'blocked'; reason: string };

/** Guard de tercera versión: ¿publicar esta revisión pisa algo que no debería? */
async function evaluarGuard(deps: ApplyDeps, revision: ApplyRevision): Promise<Guard> {
  const { store, port } = deps;
  // Un borrado sin path (tombstone de algo que nunca vimos) no toca el vault.
  if (revision.deleted && revision.path === '') return { verdict: 'already-gone' };

  const base = await store.getBase(revision.pathId);
  const actual = await port.stat(revision.path).catch(() => null);

  if (revision.kind === 'folder') {
    if (actual && actual.isFile) return { verdict: 'blocked', reason: 'un archivo local ocupa el camino' };
    return { verdict: 'safe' };
  }

  const hashActual = actual?.isFile ? sha256Hex(await port.read(revision.path)) : null;

  if (revision.deleted) {
    if (!actual) return { verdict: 'already-gone' };
    if (base?.contentHash && hashActual === base.contentHash) return { verdict: 'safe' };
    return { verdict: 'blocked', reason: 'borrado remoto contra contenido local distinto de la base' };
  }

  // add / mod de archivo
  if (!actual) {
    if (base) return { verdict: 'blocked', reason: 'el archivo local desapareció respecto de la base' };
    return { verdict: 'safe' }; // creación fresca
  }
  if (actual.isDirectory) return { verdict: 'blocked', reason: 'un directorio local ocupa el camino' };
  if (revision.contentHash && hashActual === revision.contentHash) return { verdict: 'identical' };
  if (!base) return { verdict: 'blocked', reason: 'existe localmente y no hay base (dos creaciones)' };
  if (!base.contentHash) return { verdict: 'blocked', reason: 'sin hash en la base: no puedo descartar una edición local' };
  if (hashActual === base.contentHash) return { verdict: 'safe' };
  return { verdict: 'blocked', reason: 'hay una edición local posterior a la base' };
}

/** Aplica una revisión remota. `stored` es el blob almacenado (nonce incluido) o null. */
export async function applyRevision(
  deps: ApplyDeps,
  revision: ApplyRevision,
  stored: Uint8Array | null,
): Promise<ApplyOutcome> {
  const { store, port } = deps;

  // 0) Integridad del transporte ANTES de journalizar nada.
  if (!revision.deleted && revision.kind === 'file') {
    if (!stored) throw new Error('apply: falta el blob almacenado');
    const realBlobId = sha256Hex(stored);
    if (revision.blobId && realBlobId !== revision.blobId) {
      throw new Error(`apply: blob_id no coincide (${realBlobId} != ${revision.blobId})`);
    }
  }

  // 1) Intención ANTES de escribir el temporal.
  let intent = createIntent({
    pathId: revision.pathId,
    rev: revision.rev,
    blobId: revision.blobId ?? '',
    path: revision.path,
    kind: revision.kind,
    deleted: revision.deleted,
    contentHash: revision.contentHash ?? null,
  });
  await store.putIntent(intent);

  // 2) Descifrar (AEAD) y verificar el plaintext.
  let plaintext: Uint8Array | null = null;
  let hashVerificado: string | null = null;
  if (!revision.deleted && revision.kind === 'file') {
    plaintext = decryptBlob(deps.keys, stored!);
    hashVerificado = sha256Hex(plaintext);
    if (revision.contentHash && hashVerificado !== revision.contentHash) {
      await store.deleteIntent(intent.intentId);
      throw new Error(
        `apply: content_hash no coincide (${hashVerificado} != ${revision.contentHash})`,
      );
    }
  }

  // 3) Guard de tercera versión ANTES de publicar.
  const guard = await evaluarGuard(deps, revision);
  if (guard.verdict === 'blocked') {
    intent = { ...intent, state: 'blocked', reason: guard.reason };
    await store.putIntent(intent);
    return { status: 'blocked', intentId: intent.intentId, reason: guard.reason };
  }

  // 4) Publicar.
  if (revision.kind === 'file') {
    if (revision.deleted) {
      if (guard.verdict !== 'already-gone') await port.unlink(revision.path);
    } else if (guard.verdict !== 'identical') {
      const dir = dirVault(revision.path);
      if (dir !== '.') await port.mkdirp(dir);
      const { tempName } = await port.writeTemp(dir, plaintext!);
      intent = { ...intent, tempName, contentHash: hashVerificado, state: 'verified' };
      await store.putIntent(intent);
      await port.rename(tempName, revision.path);
    }
  } else if (!revision.deleted) {
    await port.mkdirp(revision.path);
  }

  // 5) Commit atómico: cierra la intención y avanza base/índice/checkpoint.
  await store.completeApply({
    intentId: intent.intentId,
    base: {
      pathId: revision.pathId,
      rev: revision.rev,
      blobId: revision.blobId,
      contentHash: revision.deleted ? null : (revision.contentHash ?? hashVerificado),
    },
    index: {
      pathId: revision.pathId,
      path: revision.path,
      kind: revision.kind,
      deleted: revision.deleted,
      rev: revision.rev,
      blobId: revision.blobId,
      contentHash: revision.deleted ? null : (revision.contentHash ?? hashVerificado),
      size: plaintext ? plaintext.length : null,
      mtime: null,
    },
    checkpoint: { since: revision.rev, minRev: null },
  });

  return { status: 'applied', intentId: intent.intentId };
}

/** Commit de una intención ya publicada (la usan `applyRevision` y la recuperación). */
async function commitDesdeIntento(deps: ApplyDeps, intent: IntentEntry): Promise<void> {
  const { store, port } = deps;
  const stat = intent.path ? await port.stat(intent.path).catch(() => null) : null;
  await store.completeApply({
    intentId: intent.intentId,
    base: {
      pathId: intent.pathId,
      rev: intent.rev,
      blobId: intent.blobId === '' ? null : intent.blobId,
      contentHash: intent.deleted ? null : (intent.contentHash ?? null),
    },
    index: {
      pathId: intent.pathId,
      path: intent.path,
      kind: intent.kind ?? 'file',
      deleted: intent.deleted === true,
      rev: intent.rev,
      blobId: intent.blobId === '' ? null : intent.blobId,
      contentHash: intent.deleted ? null : (intent.contentHash ?? null),
      size: stat?.size ?? null,
      mtime: null,
    },
    checkpoint: { since: intent.rev, minRev: null },
  });
}

/**
 * Recuperación al reiniciar: compara cada intención con el estado REAL del filesystem.
 * - `pending`  → nada se publicó: se limpia.
 * - `verified` → hay temporal verificado y/o destino con el hash esperado: completa
 *   (publicar si falta, y commitear). Si el destino tiene cualquier otra cosa, `blocked`.
 */
export async function recoverApplies(deps: ApplyDeps): Promise<RecoveryReport> {
  const { store, port } = deps;
  const reporte: RecoveryReport = { completed: 0, cleaned: 0, blocked: 0 };
  const intents = await store.listIntents();

  for (const intent of intents) {
    if (intent.state === 'blocked') continue;

    if (intent.state === 'pending') {
      if (intent.tempName) await port.unlink(intent.tempName).catch(() => undefined);
      await store.deleteIntent(intent.intentId);
      reporte.cleaned += 1;
      continue;
    }

    // state === 'verified'
    if (!intent.tempName || !intent.path || !intent.contentHash) {
      await store.putIntent({ ...intent, state: 'blocked', reason: 'intención verificada incompleta' });
      reporte.blocked += 1;
      continue;
    }

    const destino = await port.stat(intent.path).catch(() => null);
    const hashDestino = destino?.isFile ? sha256Hex(await port.read(intent.path)) : null;

    if (hashDestino !== null && hashDestino === intent.contentHash) {
      // El rename ya había pasado: faltaba SOLO el commit.
      await commitDesdeIntento(deps, intent);
      reporte.completed += 1;
      continue;
    }

    const temp = await port.stat(intent.tempName).catch(() => null);
    if (temp?.isFile) {
      const hashTemp = sha256Hex(await port.read(intent.tempName));
      if (hashTemp !== intent.contentHash) {
        await port.unlink(intent.tempName).catch(() => undefined);
        await store.putIntent({ ...intent, state: 'blocked', reason: 'temporal no coincide con el esperado' });
        reporte.blocked += 1;
        continue;
      }
      // Publicar es seguro SOLO si el destino no cambió respecto de la base.
      const base = await store.getBase(intent.pathId);
      const destinoOk =
        hashDestino === null ? base === null : base?.contentHash != null && hashDestino === base.contentHash;
      if (!destinoOk) {
        await store.putIntent({ ...intent, state: 'blocked', reason: 'destino con cambios locales: no se pisa' });
        reporte.blocked += 1;
        continue;
      }
      await port.rename(intent.tempName, intent.path);
      await commitDesdeIntento(deps, intent);
      reporte.completed += 1;
      continue;
    }

    // Ni temporal ni destino esperado: no se adivina.
    await store.putIntent({ ...intent, state: 'blocked', reason: 'sin temporal ni destino esperado' });
    reporte.blocked += 1;
  }

  return reporte;
}

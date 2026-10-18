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
 *
 * Fase 3, paso 2: la ruta descifrada se VALIDA (portable a Windows/macOS/móvil) y el
 * directorio destino se chequea por colisiones de case: el bloqueo es explícito, con
 * motivo, nunca un skip silencioso.
 */

import { cryptoProvider, decryptBlob, type SubKeys } from '@ovaultd/crypto';
import { toHex } from '@ovaultd/protocol';

import { createIntent } from './apply.js';
import type { FileSystemPort } from './file-port.js';
import { esMergeablePorTipo, mergeDeTexto } from './merge.js';
import type { IntentEntry, StateStore } from './StateStore.js';
import type { SyncConnection } from './SyncConnection.js';
import { colisionDeCase, nombreBase, validarRutaDeVault } from './vault-path.js';

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
  /**
   * Conexión con el server. Opcional: sin ella no hay fusión 3-vías (el bloqueo por
   * "ambos cambiaron" queda igual de explícito que siempre), pero el camino normal de
   * pull la pasa siempre. Se usa SOLO para bajar el blob de la revisión base del CAS.
   */
  conn?: SyncConnection;
}

export type ApplyStatus = 'applied' | 'merged' | 'blocked' | 'noop';

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
async function evaluarGuard(
  deps: ApplyDeps,
  revision: ApplyRevision,
  /** sha256 hex del PLAINTEXT descifrado que se va a escribir (null en deletes/carpetas). */
  hashRemoto: string | null,
): Promise<Guard> {
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

  // El "esperado" es el hash del registro (si viniera) o — como la delta no manda
  // content_hash (ADR-0014) — el del PLAINTEXT DESCIFRADO que estamos por escribir:
  // un local que YA coincide con el remoto converge, no es un conflicto.
  const hashEsperado = revision.contentHash ?? hashRemoto;
  if (hashEsperado !== null && hashActual === hashEsperado) return { verdict: 'identical' };

  if (!base) return { verdict: 'blocked', reason: 'existe localmente y no hay base (dos creaciones)' };
  if (!base.contentHash) return { verdict: 'blocked', reason: 'sin hash en la base: no puedo descartar una edición local' };
  if (hashActual === base.contentHash) return { verdict: 'safe' };
  return { verdict: 'blocked', reason: 'hay una edición local posterior a la base' };
}

/**
 * Intenta una fusión 3-vías para el bloqueo "edición local posterior a la base" — el
 * único caso que es realmente de TRES versiones (base ≠ local, base ≠ remoto,
 * local ≠ remoto). Devuelve la fusión, el hash del local usado y el hash remoto; o
 * null, que significa "conflicto real" (o falta algo para fusionar) y deja el bloqueo
 * explícito de siempre. Acá no se escribe NADA: publicar es de `publicarFusion`.
 *
 * El fuzzy no decide: `mergeDeTexto` es conservador por construcción (proyecta sobre la
 * base y cualquier solapamiento o duda es conflicto).
 */
async function intentarFusionar(
  deps: ApplyDeps,
  revision: ApplyRevision,
  plaintextRemoto: Uint8Array | null,
  hashRemoto: string | null,
  reason: string,
): Promise<{ fusion: Uint8Array; oursHash: string; hashRemoto: string } | null> {
  const { store, port, keys, conn } = deps;

  // Solo el caso de tres versiones. Los demás bloqueos (ruta, colisión, sin base,
  // dos creaciones) no son fusionables por definición.
  if (reason !== 'hay una edición local posterior a la base') return null;
  if (conn === undefined || plaintextRemoto === null || hashRemoto === null) return null;
  if (revision.kind !== 'file' || revision.deleted) return null;
  if (!esMergeablePorTipo(revision.path)) return null;

  const base = await store.getBase(revision.pathId);
  if (!base?.blobId) return null;

  try {
    // La base: la última revisión sincronizada del path, del CAS y descifrada acá.
    const storedBase = await conn.getBlob(base.blobId);
    if (sha256Hex(storedBase) !== base.blobId) return null; // integridad de transporte

    const dec = new TextDecoder('utf-8', { fatal: true });
    const textoBase = dec.decode(decryptBlob(keys, storedBase));
    const bytesOurs = await port.read(revision.path);
    const oursHash = sha256Hex(bytesOurs);
    const textoOurs = dec.decode(bytesOurs);
    const textoTheirs = dec.decode(plaintextRemoto);

    const resultado = mergeDeTexto(textoBase, textoOurs, textoTheirs);
    if (resultado.tipo === 'conflicto') return null;

    return { fusion: new TextEncoder().encode(resultado.texto), oursHash, hashRemoto };
  } catch {
    // Bytes que no son UTF-8, base ilegible: fallback conservador (bloqueo explícito).
    return null;
  }
}

/**
 * Publica la fusión y cierra la aplicación. La clave está en QUÉ queda de cada lado:
 *  - el archivo local queda con la FUSIÓN (los cambios de los dos dispositivos);
 *  - base/índice avanzan a la revisión remota, con el hash REMOTO;
 *  - como el archivo difiere del índice, el scan lo ve sucio y el push siguiente sube
 *    la fusión como revisión NUEVA (expected_rev = la remota), que el otro dispositivo
 *    recibe como cualquier mod.
 */
async function publicarFusion(
  deps: ApplyDeps,
  intent: IntentEntry,
  revision: ApplyRevision,
  fusion: { fusion: Uint8Array; oursHash: string; hashRemoto: string },
): Promise<ApplyOutcome> {
  const { store, port } = deps;
  const dir = dirVault(revision.path);
  if (dir !== '.') await port.mkdirp(dir);

  const { tempName } = await port.writeTemp(dir, fusion.fusion);
  intent = {
    ...intent,
    tempName,
    contentHash: sha256Hex(fusion.fusion),
    state: 'verified',
    mergeBase: {
      rev: revision.rev,
      blobId: revision.blobId,
      contentHash: fusion.hashRemoto,
      oursHash: fusion.oursHash,
    },
  };
  await store.putIntent(intent);
  await port.rename(tempName, revision.path);

  // El commit usa `mergeBase`: base/índice quedan con el hash REMOTO (ver commitDesdeIntento).
  await commitDesdeIntento(deps, intent);
  return {
    status: 'merged',
    intentId: intent.intentId,
    reason: 'se fusionaron las ediciones de los dos dispositivos',
  };
}

/** Aplica una revisión remota. `stored` es el blob almacenado (nonce incluido) o null. */
export async function applyRevision(
  deps: ApplyDeps,
  revision: ApplyRevision,
  stored: Uint8Array | null,
  /** `forzar`: la resolución explícita del usuario pisa un bloqueo de tercera versión
   *  (la elección "usar la del server"). Los chequeos estructurales no se saltean. */
  opciones: { forzar?: boolean } = {},
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

  // 1b) La ruta DESCIFRADA se valida antes de tocar nada: traversal, absolutas, nombres
  // que otra plataforma no puede representar (plan fase 3, paso 2). Bloqueada con motivo.
  if (revision.path !== '') {
    const validez = validarRutaDeVault(revision.path);
    if (!validez.ok) {
      intent = { ...intent, state: 'blocked', reason: `ruta rechazada: ${validez.motivo}` };
      await store.putIntent(intent);
      return { status: 'blocked', intentId: intent.intentId, reason: intent.reason };
    }
  }

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

  // 3) Guard de tercera versión ANTES de publicar. Con `forzar`, un bloqueo de tercera
  // versión se convierte en una escritura DELIBERADA (la resolución explícita del usuario).
  const guard = await evaluarGuard(deps, revision, hashVerificado);
  let verdict = guard.verdict;
  if (guard.verdict === 'blocked') {
    if (opciones.forzar !== true) {
      // Antes de declarar el conflicto: si es texto y las ediciones NO se pisan, se
      // fusionan acá mismo — la resolución es el archivo con las dos, y el push
      // siguiente sube esa fusión como revisión NUEVA.
      const fusion = await intentarFusionar(deps, revision, plaintext, hashVerificado, guard.reason);
      if (fusion !== null) {
        return publicarFusion(deps, intent, revision, fusion);
      }
      intent = { ...intent, state: 'blocked', reason: guard.reason };
      await store.putIntent(intent);
      return { status: 'blocked', intentId: intent.intentId, reason: guard.reason };
    }
    verdict = 'safe';
  }

  // 3b) Colisión de case/normalización en el directorio destino: en filesystems
  // case-insensitive dos nombres que solo difieren en mayúsculas no pueden convivir.
  // Solo importa cuando vamos a ESCRIBIR (verdict `safe`); si el directorio no existe
  // todavía no hay nada con qué chocar.
  if (!revision.deleted && revision.path !== '' && verdict === 'safe') {
    const dir = dirVault(revision.path);
    try {
      const entradas = await port.list(dir);
      const choque = colisionDeCase(
        entradas.map((entrada) => entrada.name),
        nombreBase(revision.path),
      );
      if (choque !== null) {
        intent = {
          ...intent,
          state: 'blocked',
          reason: `colisión de case: «${choque}» ocupa el nombre`,
        };
        await store.putIntent(intent);
        return { status: 'blocked', intentId: intent.intentId, reason: intent.reason };
      }
    } catch {
      // Sin directorio legible no hay colisión que detectar: el mkdirp de abajo lo crea.
    }
  }

  // 4) Publicar.
  if (revision.kind === 'file') {
    if (revision.deleted) {
      if (verdict !== 'already-gone') await port.unlink(revision.path);
    } else if (verdict !== 'identical') {
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

  // Una fusión cierra contra el REMOTO (`mergeBase`), no contra el contenido del intent:
  // base/índice guardan la revisión remota y su hash, y el archivo local (la fusión)
  // queda "sucio" a propósito — el scan lo detecta y el push lo publica como revisión nueva.
  const destino = intent.mergeBase
    ? {
        rev: intent.mergeBase.rev,
        blobId: intent.mergeBase.blobId,
        contentHash: intent.mergeBase.contentHash,
      }
    : {
        rev: intent.rev,
        blobId: intent.blobId === '' ? null : intent.blobId,
        contentHash: intent.deleted ? null : (intent.contentHash ?? null),
      };

  await store.completeApply({
    intentId: intent.intentId,
    base: { pathId: intent.pathId, ...destino },
    index: {
      pathId: intent.pathId,
      path: intent.path ?? '',
      kind: intent.kind ?? 'file',
      deleted: intent.deleted === true,
      ...destino,
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
      // Publicar es seguro SOLO si el destino no cambió desde que se decidió publicar.
      // En una FUSIÓN el destino esperado es el local que se fusionó (`oursHash`): la
      // fusión fue deliberada al crear el intent, no se re-compara contra la base.
      const base = await store.getBase(intent.pathId);
      const destinoOk = intent.mergeBase
        ? hashDestino === intent.mergeBase.oursHash
        : hashDestino === null
          ? base === null
          : base?.contentHash != null && hashDestino === base.contentHash;
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

// ------------------------------------------------------------------ resolución de conflictos

export type EleccionDeResolucion = 'local' | 'remoto';

/**
 * Resuelve una intención BLOQUEADA con la elección EXPLÍCITA del usuario (resolución
 * mínima de conflictos). Dos caminos:
 *
 *  - `'remoto'`: baja el contenido del server y PISA el local (apply forzado). Quien
 *    elige esto pierde su edición local a propósito.
 *  - `'local'`: el archivo local queda INTACTO; la base avanza al remoto (con el hash
 *    del plaintext remoto) y el próximo push sube la edición local como revisión nueva.
 *
 * Ambas cierran la intención y avanzan el checkpoint hasta la revisión bloqueada: el
 * pull puede seguir. Los bloqueos ESTRUCTURALES (ruta inválida, colisión de case) no se
 * resuelven por acá — son datos que no pueden materializarse, no una tercera versión.
 */
export async function resolverBloqueado(
  deps: ApplyDeps,
  intent: IntentEntry,
  eleccion: EleccionDeResolucion,
  conn: SyncConnection,
): Promise<ApplyOutcome> {
  if (intent.state !== 'blocked') {
    throw new Error('resolver: la intención no está bloqueada');
  }

  if (eleccion === 'remoto') {
    const stored = await conn.getBlob(intent.blobId);
    return applyRevision(
      deps,
      {
        pathId: intent.pathId,
        path: intent.path ?? '',
        kind: intent.kind ?? 'file',
        deleted: intent.deleted === true,
        rev: intent.rev,
        blobId: intent.blobId === '' ? null : intent.blobId,
        contentHash: intent.contentHash ?? null,
      },
      stored,
      { forzar: true },
    );
  }

  // 'local': la base avanza al remoto SIN tocar el archivo local.
  if (intent.deleted === true) {
    throw new Error('resolver: mantener la versión local de un borrado remoto todavía no está soportado');
  }

  const stored = await conn.getBlob(intent.blobId);
  const hashRemoto = sha256Hex(decryptBlob(deps.keys, stored));
  const stat = intent.path ? await deps.port.stat(intent.path).catch(() => null) : null;

  await deps.store.completeApply({
    intentId: intent.intentId,
    base: {
      pathId: intent.pathId,
      rev: intent.rev,
      blobId: intent.blobId === '' ? null : intent.blobId,
      contentHash: hashRemoto,
    },
    index: {
      pathId: intent.pathId,
      path: intent.path ?? '',
      kind: intent.kind ?? 'file',
      deleted: false,
      rev: intent.rev,
      blobId: null,
      contentHash: hashRemoto,
      size: stat?.size ?? null,
      mtime: null,
    },
    checkpoint: { since: intent.rev, minRev: null },
  });

  return { status: 'applied', intentId: intent.intentId };
}

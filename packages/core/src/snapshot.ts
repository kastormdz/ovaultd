/**
 * El snapshot del outbox tiene dos formas según el backend: `Blob` en el plugin (IndexedDB
 * lo guarda tal cual) y `Uint8Array` en el CLI (SQLite serializa bytes, no blobs).
 *
 * Sin este punto único, cada consumidor tendría que saber dónde corre y el mismo bug
 * (`snapshot.arrayBuffer is not a function`) reaparecería en cada uno.
 */
export async function bytesDeSnapshot(
  snapshot: Blob | Uint8Array | null | undefined,
): Promise<Uint8Array | null> {
  if (!snapshot) return null;
  if (snapshot instanceof Uint8Array) return snapshot;
  return new Uint8Array(await snapshot.arrayBuffer());
}

/** Bytes del snapshot o null. */
export function tamanoDeSnapshot(snapshot: Blob | Uint8Array | null | undefined): number | null {
  if (!snapshot) return null;
  return snapshot instanceof Uint8Array ? snapshot.length : snapshot.size;
}

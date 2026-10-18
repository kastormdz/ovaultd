import { describe, expect, it } from 'vitest';

import { cryptoProvider } from '@ovaultd/crypto';
import { textEncoder, toHex } from '@ovaultd/protocol';

import { prepareOutboxEntry } from '../src/outbox.js';

/** sha256 de cero bytes: el hash legítimo de un archivo vacío. */
const hashDeVacio = (): string => toHex(cryptoProvider().sha256(new Uint8Array(0)));

const base = { path: 'nota.md', pathId: 'path-1', expectedRev: 0 };

/** base64 de un texto ASCII: sin `Buffer`, que no existe donde corre este core (navegador). */
const b64De = (texto: string): string => btoa(texto);

describe('prepareOutboxEntry', () => {
  it('acepta un archivo vacío: contentB64 vacío es contenido legítimo', () => {
    // Un .md de 0 bytes da contentB64 === ''. El chequeo original era de truthiness, así que el
    // encolado tiraba y el ciclo de sync abortaba entero: el vault no subía NADA. Caso real:
    // un vault con un archivo en blanco y 188 notas, el server quedaba con 0 revisiones.
    const entrada = prepareOutboxEntry({
      ...base,
      kind: 'add',
      contentB64: '',
      contentHash: hashDeVacio(),
    });

    expect(entrada.kind).toBe('add');
    expect(entrada.snapshot).not.toBeNull();
    expect(entrada.snapshot?.size).toBe(0);
    expect(entrada.snapshotHash).toBe(hashDeVacio());
  });

  it('un mod vacío también se encola (no sólo el alta)', () => {
    const entrada = prepareOutboxEntry({
      ...base,
      kind: 'mod',
      contentB64: '',
      contentHash: hashDeVacio(),
    });
    expect(entrada.snapshot?.size).toBe(0);
  });

  it('sin contenido sigue siendo un error: la validación no se afloja', () => {
    expect(() => prepareOutboxEntry({ ...base, kind: 'add' })).toThrow(/necesita contentB64/);
    expect(() => prepareOutboxEntry({ ...base, kind: 'mod', contentHash: 'x' })).toThrow(
      /necesita contentB64/,
    );
  });

  it('un del no lleva contenido, ni siquiera vacío', () => {
    expect(() => prepareOutboxEntry({ ...base, kind: 'del', contentB64: '' })).toThrow(
      /no lleva contenido/,
    );
    expect(() => prepareOutboxEntry({ ...base, kind: 'del', contentHash: 'x' })).toThrow(
      /no lleva contenido/,
    );
  });

  it('un archivo con contenido normal se encola con su snapshot y su hash verificado', () => {
    const bytes = textEncoder.encode('hola');
    const entrada = prepareOutboxEntry({
      ...base,
      kind: 'add',
      contentB64: b64De('hola'),
      contentHash: toHex(cryptoProvider().sha256(bytes)),
    });

    expect(entrada.snapshot?.size).toBe(4);
  });

  it('si el hash declarado no coincide con el contenido, falla', () => {
    expect(() =>
      prepareOutboxEntry({
        ...base,
        kind: 'add',
        contentB64: b64De('hola'),
        contentHash: hashDeVacio(),
      }),
    ).toThrow(/no coincide con el contenido/);
  });
});

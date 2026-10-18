/**
 * Tests de la historia y el preview (fase 5, corte 5b): descifrado de nombres,
 * verificación del hash del blob antes de descifrar, borrados y diff legible.
 */

import { describe, expect, it } from 'vitest';

import {
  blobId,
  deriveSubkeys,
  encryptBlob,
  pathEnc,
  pathId,
  randomSalt,
  randomVaultKey,
} from '@ovaultd/crypto';
import { b64Encode, toHex, type HistoryItemWire, type HistoryWire } from '@ovaultd/protocol';

import { contenidoDeRevision, diffDeTextos, historiaDeArchivo } from '../src/history.js';
import type { SyncConnection } from '../src/SyncConnection.js';

const CLAVES = deriveSubkeys(randomVaultKey(), randomSalt());
const OTRAS = deriveSubkeys(randomVaultKey(), randomSalt());
const codificador = new TextEncoder();
const decodificador = new TextDecoder();

const RUTA = 'Notas/Proyecto.md';

/** Blob cifrado + su id (hex), como los sube un cliente real. */
function blob(texto: string): { cifrado: Uint8Array; idHex: string } {
  const cifrado = encryptBlob(CLAVES, codificador.encode(texto));
  return { cifrado, idHex: toHex(blobId(cifrado)) };
}

function item(
  parcial: Partial<HistoryItemWire> & { rev: number; op: HistoryItemWire['op'] },
): HistoryItemWire {
  return { path_id: '', epoch: 1, ts: '2026-10-18 12:00:00', ...parcial };
}

/** Conexión fake: una página fija de historia + blobs por hash. */
function connFake(pagina: HistoryWire, blobs: Map<string, Uint8Array>): SyncConnection {
  return {
    async historial() {
      return pagina;
    },
    async getBlob(blobIdHex: string) {
      const bytes = blobs.get(blobIdHex);
      if (!bytes) throw new Error(`blob ${blobIdHex} no existe`);
      return bytes;
    },
    async pull() {
      throw new Error('pull no debería llamarse desde la historia');
    },
  } as unknown as SyncConnection;
}

describe('historiaDeArchivo', () => {
  it('descifra los nombres y mapea los campos (add, del sin nombre)', async () => {
    const contenido = blob('uno');
    const pathIdHex = toHex(pathId(CLAVES, RUTA));
    const pagina: HistoryWire = {
      items: [
        item({
          rev: 3,
          op: 'mod',
          path_id: pathIdHex,
          path_enc: b64Encode(pathEnc(CLAVES, RUTA)),
          blob_hash: contenido.idHex,
          size: 5,
          device_id: 2,
        }),
        item({ rev: 2, op: 'del', path_id: pathIdHex }),
        item({
          rev: 1,
          op: 'add',
          path_id: pathIdHex,
          path_enc: b64Encode(pathEnc(CLAVES, RUTA)),
          blob_hash: contenido.idHex,
          size: 5,
        }),
      ],
      next_before: null,
      min_rev: 0,
    };

    const resultado = await historiaDeArchivo({
      conn: connFake(pagina, new Map()),
      keys: CLAVES,
      ruta: RUTA,
    });

    expect(resultado.items).toHaveLength(3);
    expect(resultado.items[0]!.ruta).toBe(RUTA);
    expect(resultado.items[0]!.op).toBe('mod');
    expect(resultado.items[0]!.blobId).toBe(contenido.idHex);
    expect(resultado.items[0]!.deviceId).toBe(2);
    expect(resultado.items[1]!.op).toBe('del');
    expect(resultado.items[1]!.ruta).toBeNull(); // un del no trae nombre
    expect(resultado.items[1]!.blobId).toBeNull();
    expect(resultado.nextBefore).toBeNull();
    expect(resultado.minRev).toBe(0);
  });

  it('un nombre que no autentica queda null (no revienta la lista)', async () => {
    const pathIdHex = toHex(pathId(CLAVES, RUTA));
    const pagina: HistoryWire = {
      items: [
        item({
          rev: 1,
          op: 'add',
          path_id: pathIdHex,
          path_enc: b64Encode(pathEnc(OTRAS, RUTA)), // cifrado con OTRAS claves
        }),
      ],
      next_before: null,
      min_rev: 0,
    };

    const resultado = await historiaDeArchivo({
      conn: connFake(pagina, new Map()),
      keys: CLAVES,
      ruta: RUTA,
    });
    expect(resultado.items[0]!.ruta).toBeNull();
  });
});

describe('contenidoDeRevision', () => {
  it('verifica el hash y devuelve el contenido descifrado', async () => {
    const { cifrado, idHex } = blob('hola historia');
    const conn = connFake({ items: [], next_before: null, min_rev: 0 }, new Map([[idHex, cifrado]]));

    const contenido = await contenidoDeRevision({ conn, keys: CLAVES, blobId: idHex });
    expect(decodificador.decode(contenido)).toBe('hola historia');
  });

  it('un blob que no hashea a su id se rechaza ANTES de descifrar', async () => {
    const { idHex } = blob('original');
    const otro = blob('otro contenido');
    const conn = connFake(
      { items: [], next_before: null, min_rev: 0 },
      new Map([[idHex, otro.cifrado]]),
    );

    await expect(contenidoDeRevision({ conn, keys: CLAVES, blobId: idHex })).rejects.toThrow(
      'no coincide con su hash',
    );
  });
});

describe('diffDeTextos', () => {
  it('marca altas y bajas por línea, sin contexto', () => {
    const diff = diffDeTextos('uno\ndos\ntres\n', 'uno\ndos cambiado\ntres\n');
    expect(diff).toBe('- dos\n+ dos cambiado');
  });

  it('sin cambios no hay diff', () => {
    expect(diffDeTextos('igual\n', 'igual\n')).toBe('');
  });
});

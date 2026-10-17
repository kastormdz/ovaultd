/**
 * Tests del export (fase 3d): colapso del delta al estado vivo, validación de rutas,
 * omisión de existentes y descifrado real (blobs cifrados con las subclaves del test).
 */

import { describe, expect, it } from 'vitest';

import { blobId, deriveSubkeys, encryptBlob, pathEnc, pathId, randomSalt, randomVaultKey } from '@ovaultd/crypto';
import { b64Encode, toHex, type SignedRecordWire } from '@ovaultd/protocol';

import { exportarVault, type PuertoDeExport } from '../src/export.js';
import type { SyncConnection } from '../src/SyncConnection.js';

const CLAVES = deriveSubkeys(randomVaultKey(), randomSalt());
const codificador = new TextEncoder();
const decodificador = new TextDecoder();

/** Blob cifrado + su id (hex), como los sube un cliente real. */
function blob(texto: string): { cifrado: Uint8Array; idHex: string } {
  const cifrado = encryptBlob(CLAVES, codificador.encode(texto));
  return { cifrado, idHex: toHex(blobId(cifrado)) };
}

function registro(
  op: 'add' | 'mod' | 'del',
  ruta: string,
  blobIdB64: string,
  rev: number,
): SignedRecordWire {
  return {
    rev,
    op,
    path_id: b64Encode(pathId(CLAVES, ruta)),
    blob_id: blobIdB64,
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

/** Conexión fake: entrega las páginas de a una y después vacío. */
function connFake(
  paginas: SignedRecordWire[][],
  blobs: Map<string, Uint8Array>,
): SyncConnection {
  let pagina = 0;
  return {
    async pull(since: number) {
      if (pagina >= paginas.length) {
        return { items: [], next_since: since, min_rev: null };
      }
      const items = paginas[pagina]!;
      pagina += 1;
      return { items, next_since: since + items.length, min_rev: null };
    },
    async getBlob(hex: string) {
      const bytes = blobs.get(hex);
      if (!bytes) throw new Error(`blob ausente: ${hex}`);
      return bytes;
    },
  } as unknown as SyncConnection;
}

function puertoMemoria(preExistentes: string[] = []): {
  puerto: PuertoDeExport;
  archivos: Map<string, Uint8Array>;
} {
  const archivos = new Map<string, Uint8Array>();
  for (const ruta of preExistentes) archivos.set(ruta, new Uint8Array());
  return {
    archivos,
    puerto: {
      async existe(ruta) {
        return archivos.has(ruta);
      },
      async escribir(ruta, contenido) {
        archivos.set(ruta, contenido);
      },
    },
  };
}

const CARPETA = b64Encode(new Uint8Array(32)); // el centinela de "sin blob"

describe('exportarVault', () => {
  it('colapsa el delta al estado vivo y descifra', async () => {
    const a1 = blob('hola v1');
    const a2 = blob('hola v2 (final)');
    const b = blob('nota b');
    const blobs = new Map([
      [a1.idHex, a1.cifrado],
      [a2.idHex, a2.cifrado],
      [b.idHex, b.cifrado],
    ]);

    // Página 1: add a, mod a, del x, carpeta; Página 2: add b.
    const conn = connFake(
      [
        [
          registro('add', 'Notas/a.md', b64Encode(hexABytes(a1.idHex)), 1),
          registro('mod', 'Notas/a.md', b64Encode(hexABytes(a2.idHex)), 2),
          registro('add', 'x.md', b64Encode(hexABytes(a1.idHex)), 3),
          registro('del', 'x.md', CARPETA, 4),
          registro('add', 'Carpeta', CARPETA, 5),
        ],
        [registro('add', 'b.md', b64Encode(hexABytes(b.idHex)), 6)],
      ],
      blobs,
    );

    const { puerto, archivos } = puertoMemoria();
    const resultado = await exportarVault({ conn, keys: CLAVES, puerto });

    // a.md queda con la v2, b.md existe, x.md (borrada) no, la carpeta no se materializa.
    expect(resultado.escritas).toBe(2);
    expect(resultado.bloqueadas).toBe(0);
    expect(decodificador.decode(archivos.get('Notas/a.md'))).toBe('hola v2 (final)');
    expect(decodificador.decode(archivos.get('b.md'))).toBe('nota b');
    expect(archivos.has('x.md')).toBe(false);
    expect(archivos.has('Carpeta')).toBe(false);
    expect(resultado.bytes).toBe('hola v2 (final)'.length + 'nota b'.length);
  });

  it('rechaza rutas hostiles antes de escribir', async () => {
    const evil = blob('escapando');
    const blobs = new Map([[evil.idHex, evil.cifrado]]);

    const conn = connFake(
      [[registro('add', '../escape.md', b64Encode(hexABytes(evil.idHex)), 1)]],
      blobs,
    );

    const { puerto, archivos } = puertoMemoria();
    const resultado = await exportarVault({ conn, keys: CLAVES, puerto });

    expect(resultado.bloqueadas).toBe(1);
    expect(resultado.escritas).toBe(0);
    expect(archivos.size).toBe(0);
  });

  it('omite lo que ya existe (y lo reescribe con omitirExistentes: false)', async () => {
    const n = blob('nota nueva');
    const blobs = new Map([[n.idHex, n.cifrado]]);

    const conn1 = connFake([[registro('add', 'n.md', b64Encode(hexABytes(n.idHex)), 1)]], blobs);
    const omitido = puertoMemoria(['n.md']);
    const r1 = await exportarVault({ conn: conn1, keys: CLAVES, puerto: omitido.puerto });
    expect(r1.omitidas).toBe(1);
    expect(r1.escritas).toBe(0);

    const conn2 = connFake([[registro('add', 'n.md', b64Encode(hexABytes(n.idHex)), 1)]], blobs);
    const pisado = puertoMemoria(['n.md']);
    const r2 = await exportarVault({
      conn: conn2,
      keys: CLAVES,
      puerto: pisado.puerto,
      omitirExistentes: false,
    });
    expect(r2.escritas).toBe(1);
    expect(decodificador.decode(pisado.archivos.get('n.md'))).toBe('nota nueva');
  });
});

/** hex (64 chars) → bytes, para pasar ids por b64 como los pasa el wire. */
function hexABytes(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i += 1) {
    bytes[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

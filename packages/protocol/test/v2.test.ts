/**
 * Conformidad v2 — el MISMO fixture que ejecuta el lado Rust
 * (`crates/ovaultd-protocol/tests/v2_conformance.rs`): `tests/fixtures/protocol/v2.json`.
 */

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

import {
  ERROR_CODES,
  b64Decode,
  b64Encode,
  checkContentSize,
  fromHex,
  opFromWire,
  opToWire,
  operationIdFromWire,
  toHex,
  validateBatch,
  type BatchWire,
  type OpCode,
} from '../src/index.js';

const fixture = JSON.parse(
  readFileSync(new URL('../../../tests/fixtures/protocol/v2.json', import.meta.url), 'utf8'),
) as {
  op_wire: Array<{ wire?: string; wire_numeric?: number; code: number }>;
  op_wire_invalid: unknown[];
  error_codes: string[];
  content_size: Array<{ size: number; ok: boolean }>;
  operation_id_from_wire: Array<{ wire: string; bytes_hex?: string; invalid?: boolean }>;
  id_b64: Array<{ name: string; bytes_hex: string; b64: string }>;
  batch_examples: Array<{ name: string; result: string; batch: BatchWire }>;
};

describe('conformidad v2', () => {
  it('mapea operaciones del wire en ambos sentidos', () => {
    for (const caso of fixture.op_wire) {
      if (caso.wire !== undefined) {
        const code = opFromWire(caso.wire);
        expect(code, `op ${caso.wire}`).toBe(caso.code);
        expect(opToWire(code as OpCode)).toBe(caso.wire);
      } else {
        expect(opFromWire(caso.wire_numeric)).toBe(caso.code);
      }
    }
  });

  it('rechaza operaciones inválidas', () => {
    for (const invalido of fixture.op_wire_invalid) {
      expect(opFromWire(invalido), `inválida ${String(invalido)}`).toBeNull();
    }
  });

  it('congela la lista cerrada de códigos de error', () => {
    expect([...ERROR_CODES]).toEqual(fixture.error_codes);
  });

  it('aplica el tope de contenido', () => {
    for (const caso of fixture.content_size) {
      expect(checkContentSize(caso.size), `size ${caso.size}`).toBe(caso.ok ? null : 'max_file_exceeded');
    }
  });

  it('parsea operation_id del wire (hex con guiones tolerados)', () => {
    for (const caso of fixture.operation_id_from_wire) {
      if (caso.invalid) {
        expect(() => operationIdFromWire(caso.wire), caso.wire).toThrow();
      } else {
        expect(toHex(operationIdFromWire(caso.wire)), caso.wire).toBe(caso.bytes_hex);
      }
    }
  });

  it('serializa identificadores en base64 estándar', () => {
    for (const caso of fixture.id_b64) {
      const bytes = fromHex(caso.bytes_hex);
      expect(b64Encode(bytes), caso.name).toBe(caso.b64);
      expect(toHex(b64Decode(caso.b64)), caso.name).toBe(caso.bytes_hex);
    }
  });

  it('valida los batch de ejemplo igual que el server', () => {
    for (const caso of fixture.batch_examples) {
      const resultado = validateBatch(caso.batch) ?? 'ok';
      expect(resultado, `batch ${caso.name}`).toBe(caso.result);
    }
  });

  it('rechaza base64 inválido', () => {
    for (const invalido of ['no es base64!!', 'AAAA=', 'A===', 'AB=C', 'A']) {
      expect(() => b64Decode(invalido), invalido).toThrow();
    }
  });
});

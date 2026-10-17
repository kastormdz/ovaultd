/**
 * Helpers canónicos del wire v2: bytes, base64, hex y NFKC.
 *
 * Reglas (`docs/protocol.md`, `docs/implementation-map.md` § 6):
 * - Todo campo binario viaja en JSON como **base64 estándar con padding** (RFC 4648).
 * - El `operation_id` del batch viaja como **hex con guiones tolerados**; la delta lo
 *   devuelve en **base64** (16 B). El parser ÚNICO de la forma textual es
 *   `operationIdFromWire` (mismo comportamiento que `OperationId::from_wire` en Rust).
 * - Hex es para humanos (logs, CLI); el protocolo jamás lo usa salvo el caso anterior.
 *
 * Implementación sin dependencias y sin `Buffer`: el mismo código corre en Node
 * (CLI) y en el navegador (plugin de Obsidian).
 */

export const textEncoder = new TextEncoder();
export const textDecoder = new TextDecoder();

/** Normalización NFKC — la ÚNICA forma válida de passwords y paths antes de cifrar/firmar. */
export const nfkc = (text: string): string => text.normalize('NFKC');

export const concat = (...parts: Uint8Array[]): Uint8Array => {
  let total = 0;
  for (const part of parts) total += part.length;
  const out = new Uint8Array(total);
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
};

export const equalBytes = (a: Uint8Array, b: Uint8Array): boolean => {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= a[i]! ^ b[i]!;
  return diff === 0;
};

export const u8 = (value: number): Uint8Array => Uint8Array.of(value & 0xff);

export const u16be = (value: number): Uint8Array => {
  const out = new Uint8Array(2);
  new DataView(out.buffer).setUint16(0, value, false);
  return out;
};

export const u32be = (value: number): Uint8Array => {
  const out = new Uint8Array(4);
  new DataView(out.buffer).setUint32(0, value, false);
  return out;
};

export const u64be = (value: number | bigint): Uint8Array => {
  const out = new Uint8Array(8);
  new DataView(out.buffer).setBigUint64(0, BigInt(value), false);
  return out;
};

export const toHex = (bytes: Uint8Array): string => {
  let out = '';
  for (const byte of bytes) out += byte.toString(16).padStart(2, '0');
  return out;
};

export const fromHex = (text: string): Uint8Array => {
  if (text.length % 2 !== 0) throw new Error('hex con longitud impar');
  const out = new Uint8Array(text.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    const pair = text.slice(i * 2, i * 2 + 2);
    if (!/^[0-9a-fA-F]{2}$/.test(pair)) throw new Error(`hex inválido: ${pair}`);
    out[i] = Number.parseInt(pair, 16);
  }
  return out;
};

const B64_ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

const B64_DECODE = (() => {
  const table = new Int16Array(128).fill(-1);
  for (let i = 0; i < B64_ALPHABET.length; i += 1) table[B64_ALPHABET.charCodeAt(i)] = i;
  return table;
})();

const b64CharValue = (text: string, index: number): number => {
  const code = text.charCodeAt(index);
  const value = code < 128 ? B64_DECODE[code]! : -1;
  if (value < 0) throw new Error(`base64 inválido: carácter en posición ${index}`);
  return value;
};

/** Codifica base64 estándar con padding (el mismo motor que el server: STANDARD). */
export const b64Encode = (bytes: Uint8Array): string => {
  let out = '';
  const len = bytes.length;
  for (let i = 0; i < len; i += 3) {
    const b0 = bytes[i]!;
    const has1 = i + 1 < len;
    const has2 = i + 2 < len;
    const b1 = has1 ? bytes[i + 1]! : 0;
    const b2 = has2 ? bytes[i + 2]! : 0;
    out += B64_ALPHABET[b0 >> 2];
    out += B64_ALPHABET[((b0 & 0x03) << 4) | (b1 >> 4)];
    out += has1 ? B64_ALPHABET[((b1 & 0x0f) << 2) | (b2 >> 6)] : '=';
    out += has2 ? B64_ALPHABET[b2 & 0x3f] : '=';
  }
  return out;
};

/**
 * Decodifica base64 estándar. Estricto como el motor del server: longitud múltiplo de 4,
 * padding solo al final, bits de padding en cero. Cualquier otra cosa lanza.
 */
export const b64Decode = (text: string): Uint8Array => {
  if (text.length % 4 !== 0) throw new Error('base64 inválido: longitud no múltiplo de 4');
  const out: number[] = [];
  for (let i = 0; i < text.length; i += 4) {
    const last = i + 4 === text.length;
    const c0 = b64CharValue(text, i);
    const c1 = b64CharValue(text, i + 1);
    const p2 = text[i + 2]!;
    const p3 = text[i + 3]!;
    if (!last && (p2 === '=' || p3 === '=')) {
      throw new Error('base64 inválido: padding fuera del final');
    }
    if (p2 === '=' && p3 !== '=') throw new Error('base64 inválido: padding incompleto');

    out.push((c0 << 2) | (c1 >> 4));

    if (p2 !== '=') {
      const c2 = b64CharValue(text, i + 2);
      out.push(((c1 & 0x0f) << 4) | (c2 >> 2));
      if (p3 !== '=') {
        const c3 = b64CharValue(text, i + 3);
        out.push(((c2 & 0x03) << 6) | c3);
      } else if ((c2 & 0x03) !== 0) {
        throw new Error('base64 inválido: bits de padding en cero requeridos');
      }
    } else if ((c1 & 0x0f) !== 0) {
      throw new Error('base64 inválido: bits de padding en cero requeridos');
    }
  }
  return Uint8Array.from(out);
};

/** Valida y devuelve un identificador binario de longitud fija. */
export const expectBytes = (bytes: Uint8Array, length: number, what: string): Uint8Array => {
  if (bytes.length !== length) {
    throw new Error(`${what}: se esperaban ${length} bytes, llegaron ${bytes.length}`);
  }
  return bytes;
};

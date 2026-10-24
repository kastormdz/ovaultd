/**
 * Mensajes y constantes del protocolo v2 — el contrato tal como lo habla el server.
 *
 * Fuente de verdad: `crates/ovaultd-protocol` (Rust) y `docs/server-interfaces.md`.
 * Este archivo NO inventa: cada campo, forma y error corresponde al comportamiento
 * verificado del server (los DTOs reales de las rutas, no la prosa de los docs).
 */

import { textEncoder } from './canonical.js';

// ---------------------------------------------------------------------------
// Constantes de protocolo
// ---------------------------------------------------------------------------

/** Versión de cifrado del protocolo. */
export const ENC_VERSION = 'sync2';

/** Tope de **contenido** (plaintext) por archivo (ADR-0007). */
export const MAX_FILE_BYTES = 33_554_432;

/**
 * Sobre del blob almacenado: `nonce[24] ‖ ciphertext ‖ tag[16]` (ADR-0014).
 * El cuerpo cifrado que viaja por el wire es el blob almacenado: el tope del
 * cuerpo es el tope de contenido + estos 40 bytes.
 */
export const BLOB_ENVELOPE_BYTES = 40;

/** Tope de operaciones por batch: superarlo es `bad_request`. */
export const MAX_BATCH_OPS = 1000;

// ---------------------------------------------------------------------------
// Operaciones
// ---------------------------------------------------------------------------

/**
 * Discriminantes del enum `Op` en el JSON del wire (la forma numérica que el `Deserialize`
 * tolera; la forma textual `"add"`/`"mod"`/… es la que usamos).
 */
export const OP_ADD = 1;
export const OP_MOD = 2;
export const OP_DEL = 3;
export const OP_MOVE = 4;

/**
 * Códigos de operación que firma `msg_rec` — el mapeo de `commit.rs::verify_batch_operation`
 * (`Add => 0, Mod => 1, Del => 2, Move => 3`). **NO** son los discriminantes del enum:
 * firmar con `OP_ADD` (1) produce una firma que el server rechaza.
 */
export const SIGN_OP_ADD = 0;
export const SIGN_OP_MOD = 1;
export const SIGN_OP_DEL = 2;
export const SIGN_OP_MOVE = 3;

/** Código de firma para una operación del wire. */
export const signOpCode = (op: OpWire): number => {
  switch (op) {
    case 'add':
      return SIGN_OP_ADD;
    case 'mod':
      return SIGN_OP_MOD;
    case 'del':
      return SIGN_OP_DEL;
    case 'move':
      return SIGN_OP_MOVE;
  }
};

export type OpCode = typeof OP_ADD | typeof OP_MOD | typeof OP_DEL | typeof OP_MOVE;
export type OpWire = 'add' | 'mod' | 'del' | 'move';

const OP_WIRE_BY_CODE: Record<OpCode, OpWire> = {
  [OP_ADD]: 'add',
  [OP_MOD]: 'mod',
  [OP_DEL]: 'del',
  [OP_MOVE]: 'move',
};

const OP_CODE_BY_WIRE: Record<string, OpCode> = {
  add: OP_ADD,
  mod: OP_MOD,
  del: OP_DEL,
  move: OP_MOVE,
};

/** Convierte un código de operación a su forma del wire. */
export const opToWire = (code: OpCode): OpWire => OP_WIRE_BY_CODE[code];

/**
 * Parsea la forma del wire de una operación: string (`"add"`) o código numérico.
 * Equivale al `Deserialize` de `Op` en Rust; devuelve `null` si no es válida.
 */
export const opFromWire = (value: unknown): OpCode | null => {
  if (typeof value === 'string') return OP_CODE_BY_WIRE[value] ?? null;
  if (typeof value === 'number' && Number.isInteger(value)) {
    return value === OP_ADD || value === OP_MOD || value === OP_DEL || value === OP_MOVE
      ? (value as OpCode)
      : null;
  }
  return null;
};

// ---------------------------------------------------------------------------
// Códigos de error (lista cerrada — ampliarla requiere un ADR)
// ---------------------------------------------------------------------------

export const ERROR_CODES = [
  'unauthorized',
  'not_found',
  'bad_request',
  'max_file_exceeded',
  'quota_exceeded',
  'resync_required',
  'internal',
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

export const isErrorCode = (value: unknown): value is ErrorCode =>
  typeof value === 'string' && (ERROR_CODES as readonly string[]).includes(value);

// ---------------------------------------------------------------------------
// OperationId: hex con guiones tolerados (batch) ⇄ 16 bytes (delta lo devuelve b64)
// ---------------------------------------------------------------------------

/**
 * Parsea la forma textual del wire de un `operation_id`: hexadecimal con guiones
 * tolerados. Es el port exacto de `OperationId::from_wire` (Rust): la escritura
 * (batch) y la lectura (delta) comparten este único parser.
 *
 * Lanza si hay caracteres inválidos, si sobra o falta algún dígito.
 */
export const operationIdFromWire = (text: string): Uint8Array => {
  const bytes = new Uint8Array(16);
  let index = 0;
  let high: number | null = null;

  for (const byte of textEncoder.encode(text)) {
    if (byte === 0x2d) continue; // '-'
    let nibble: number;
    if (byte >= 0x30 && byte <= 0x39) nibble = byte - 0x30;
    else if (byte >= 0x61 && byte <= 0x66) nibble = byte - 0x61 + 10;
    else if (byte >= 0x41 && byte <= 0x46) nibble = byte - 0x41 + 10;
    else throw new Error('operation_id con carácter inválido');

    if (high !== null) {
      if (index >= bytes.length) throw new Error('operation_id demasiado largo');
      bytes[index] = (high << 4) | nibble;
      index += 1;
      high = null;
    } else {
      high = nibble;
    }
  }

  if (index === bytes.length && high === null) return bytes;
  throw new Error('operation_id incompleto');
};

// ---------------------------------------------------------------------------
// Validación de contenido (espejo de `check_content_size` + `Batch::validate`)
// ---------------------------------------------------------------------------

/** Devuelve el `ErrorCode` del tope de contenido, o `null` si entra. */
export const checkContentSize = (size: number): ErrorCode | null =>
  size > MAX_FILE_BYTES ? 'max_file_exceeded' : null;

/** Operación del batch tal como viaja en JSON (campos binarios en base64). */
export interface BatchOpWire {
  op: OpWire;
  /** path_id (32 B) en base64. */
  path_id: string;
  /** path_enc en base64; ausente solo en `del`. */
  path_enc?: string | null;
  folder: boolean;
  /** blob_id (32 B) en base64. */
  blob_id?: string | null;
  /** t_mac (64 B) en base64. */
  t_mac?: string | null;
  /** Firma (64 B) en base64. */
  sig: string;
  size?: number | null;
  mtime?: number | null;
  ctime?: number | null;
  prev_path_id?: string | null;
}

/** El sobre: N operaciones, UN expected_rev, UNA revisión. */
export interface BatchWire {
  t?: string;
  batch_id: string;
  operation_id: string;
  expected_rev: number;
  ops: BatchOpWire[];
}

/** Espejo de `BatchOp::validate` — misma tabla de restricciones. */
export const validateBatchOp = (op: BatchOpWire): ErrorCode | null => {
  const hasBlob = op.blob_id !== null && op.blob_id !== undefined;
  const hasPathEnc = op.path_enc !== null && op.path_enc !== undefined;
  const hasSize = op.size !== null && op.size !== undefined;
  const hasPrev = op.prev_path_id !== null && op.prev_path_id !== undefined;

  switch (op.op) {
    case 'del':
      if (hasBlob || hasPathEnc) return 'bad_request';
      return null;
    case 'add':
    case 'mod': {
      if (!hasPathEnc) return 'bad_request';
      if (op.folder) {
        if (hasBlob || hasSize) return 'bad_request';
      } else {
        if (!hasBlob || !hasSize) return 'bad_request';
        const sizeCode = checkContentSize(op.size!);
        if (sizeCode) return sizeCode;
      }
      return null;
    }
    case 'move': {
      if (!hasPathEnc || !hasPrev) return 'bad_request';
      if (op.folder) {
        if (hasBlob || hasSize) return 'bad_request';
      } else {
        if (!hasBlob || !hasSize) return 'bad_request';
        const sizeCode = checkContentSize(op.size!);
        if (sizeCode) return sizeCode;
      }
      return null;
    }
    default:
      return 'bad_request';
  }
};

/** Espejo de `Batch::validate` — vacío, techo de ops, cada op y firma no vacía. */
export const validateBatch = (batch: BatchWire): ErrorCode | null => {
  if (batch.ops.length === 0) return 'bad_request';
  if (batch.ops.length > MAX_BATCH_OPS) return 'bad_request';
  for (const op of batch.ops) {
    const code = validateBatchOp(op);
    if (code) return code;
    if (typeof op.sig !== 'string' || op.sig.length === 0) return 'bad_request';
  }
  return null;
};

// ---------------------------------------------------------------------------
// DTOs HTTP (formas exactas de las rutas — behavior, no prosa)
// ---------------------------------------------------------------------------

export interface KdfParamsWire {
  m_kib: number;
  t: number;
  p: number;
  dk_len: number;
}

export interface SigninResponse {
  token: string;
  device_id: number;
}

export interface DeviceSummary {
  id: number;
  name: string;
  platform: string | null;
  created_at: string;
  revoked_at: string | null;
}

export interface MeResponse {
  account_id: number;
  email: string;
  devices: DeviceSummary[];
}

export interface VaultSummary {
  id: number;
  name: string;
  role: string;
  rev: number;
}

/** `GET /vaults/{id}/material` (ADR-0013): solo material público. */
export interface VaultMaterial {
  id: number;
  vault_id: string | null;
  name: string;
  enc_version: number | string;
  epoch: number;
  salt: string;
  kdf_alg: string;
  kdf_params: string;
  wrapped_key_pw: string;
  wrapped_key_rc: string;
  vault_pubkey: string;
}

/** `POST /vaults`: el cuerpo que arma el cliente (el server no ve secretos). */
export interface CreateVaultWire {
  name: string;
  salt: string;
  kdf_params: KdfParamsWire;
  wrapped_key_pw: string;
  wrapped_key_rc?: string;
  vault_pubkey: string;
  /** UUID de 16 B del cliente (ADR-0013), en base64. */
  vault_id?: string;
  max_file_bytes?: number;
}

export interface ChallengeResponse {
  challenge: string;
}

export interface AccessResponse {
  host: string;
  rev: number;
}

export interface BatchAckWire {
  t?: string;
  batch_id: string;
  rev: number;
}

/** Registro firmado tal como lo devuelve la delta (`GET /vaults/{id}/changes`). */
export interface SignedRecordWire {
  rev: number;
  op: OpWire;
  path_id: string;
  blob_id: string;
  path_enc: string;
  t_mac: string;
  sig: string;
  size: number;
  mtime: number;
  expected_rev: number;
  /** 16 B en **base64** (la delta serializa `OperationId` con su forma del wire). */
  operation_id: string;
  epoch: number;
  prev_path_id?: string | null;
}

export interface ChangesWire {
  items: SignedRecordWire[];
  next_since: number;
  min_rev: number | null;
}

/**
 * Un ítem de la historia (`GET /vaults/{id}/history`): una fila del oplog, sin descifrar.
 * `path_id`/`prev_path_id`/`blob_hash` viajan en hex; `path_enc`/`sig`/`t_mac` en base64.
 */
export interface HistoryItemWire {
  rev: number;
  op: OpWire;
  path_id: string;
  path_enc?: string;
  prev_path_id?: string;
  blob_hash?: string;
  sig?: string;
  t_mac?: string;
  size?: number;
  mtime?: number;
  device_id?: number;
  epoch: number;
  ts: string;
}

export interface HistoryWire {
  items: HistoryItemWire[];
  /** `rev` para seguir paginando hacia atrás (`before`); `null` = no hay más. */
  next_before: number | null;
  /** Piso de retención del vault: por debajo, la historia ya no está. */
  min_rev: number;
}

/** Sobre de error del wire: `{ "t": "error", "code": ..., "detail": ... }`. */
export interface ErrorWire {
  t: string;
  code: string;
  detail: string;
}

// ---------------------------------------------------------------------------
// WebSocket (handshake y canal)
// ---------------------------------------------------------------------------

export interface HelloWire {
  t: 'hello';
  proto: number;
  vault: number;
  device: number;
  since: number;
  /** Versión del cliente (`plugin 0.2.34`, `cli 0.1.0`). El server la guarda por device. */
  app_version?: string;
  modes?: { sync: string; conflict: string };
}

export interface HelloOkWire {
  t: 'hello_ok';
  challenge: string;
  head_rev: number;
}

export interface AuthWire {
  t: 'auth';
  proof: string;
}

export interface AuthOkWire {
  t: 'auth_ok';
  rev: number;
}

export interface AckWire {
  t: 'ack';
  rev: number;
}

export interface NotifyWire {
  t: 'notify';
  rev: number;
  op: OpWire;
  path_id: string;
  blob_id: string;
  t_mac: string;
  sig: string;
  size: number;
  device: string;
  deleted: boolean;
  prev_path_id?: string | null;
}

export interface BlobHeadWire {
  t: 'blob_head';
  rev: number;
  transfer: string;
  len: number;
  offset: number;
}

export interface ConflictWire {
  t: 'conflict';
  path_id: string;
  base_rev: number;
  server_rev: number;
  client_rev: number;
}

/** Mensajes que el servidor puede enviar por WS. */
export type ServerMessageWire =
  | HelloOkWire
  | AuthOkWire
  | AckWire
  | NotifyWire
  | BlobHeadWire
  | ConflictWire
  | BatchAckWire
  | ErrorWire;

/** Mensajes que el cliente envía por WS. */
export type ClientMessageWire = HelloWire | AuthWire | BatchWire;

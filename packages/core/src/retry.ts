/**
 * Reintentos con backoff (plan fase 2, paso 4): `2^n × 5 s`, cap 5 min, con jitter, y
 * clasificación de errores: qué se reintenta, qué es fatal y qué pide resync.
 */

export type RetryKind = 'transient' | 'fatal' | 'resync';

export const BACKOFF_BASE_MS = 5_000;
export const BACKOFF_CAP_MS = 300_000;

/** Backoff `2^n × 5 s` con cap y jitter (mitad fija, mitad aleatoria). */
export function backoffMs(intento: number, random: () => number = Math.random): number {
  const base = Math.min(BACKOFF_BASE_MS * 2 ** Math.max(0, intento), BACKOFF_CAP_MS);
  return Math.round(base / 2 + (base / 2) * random());
}

/** Errores con `status`/`code` del wire (los lanza SyncConnection). */
export interface Clasificable {
  status?: number;
  code?: string | null;
}

/** Clasifica un error para decidir si se reintenta, se corta o pide resync. */
const RECHAZOS_FATALES = new Set([
  'bad_request',
  'max_file_exceeded',
  'quota_exceeded',
  'not_found',
  'unauthorized',
]);

export function clasificarError(error: unknown): RetryKind {
  const e = (error ?? {}) as Clasificable;
  if (e.code === 'resync_required') return 'resync';
  // `SyncRejectedError` trae `code` pero NO `status`: un rechazo del wire era "transitorio"
  // y se reintentaba para siempre. `internal` queda afuera a propósito: puede ser pasajero.
  if (
    e.status === undefined &&
    typeof e.code === 'string' &&
    RECHAZOS_FATALES.has(e.code)
  ) {
    return 'fatal';
  }
  if (!e.status) return 'transient'; // falla de red/WS: se reintenta
  if (e.status === 429 || e.status >= 500) return 'transient';
  return 'fatal'; // 4xx restantes (401, 403, 404, bad_request…)
}

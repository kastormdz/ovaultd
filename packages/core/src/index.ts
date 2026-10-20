/**
 * `@ovaultd/core` — núcleo durable del cliente (fase 2).
 *
 * Corre DENTRO del runtime Chromium (IndexedDB como único backend de estado).
 * No conoce el filesystem ni el wire: eso son `NodeFileAdapter` (CLI) y
 * `SyncConnection` (que se agregan encima de esta base).
 */

export * from './StateStore.js';
export * from './outbox.js';
export * from './apply.js';
export * from './apply-runner.js';
export * from './file-port.js';
export * from './retry.js';
export * from './scan.js';
export * from './push.js';
export * from './prune.js';
export * from './apariencia.js';
export * from './SyncConnection.js';
export * from './reconcile.js';
export * from './SyncClient.js';
export * from './vault-path.js';
export * from './SyncFilter.js';
export * from './merge.js';
export * from './export.js';
export * from './history.js';

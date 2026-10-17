/**
 * `BrowserRuntime` — el runtime durable del CLI: Chromium headless con perfil
 * persistente propio, sirviendo el bundle del núcleo desde un origen local estable.
 *
 * Decisiones (del plan de fase 2):
 *  - **Un solo motor durable**: IndexedDB vive acá; no hay SQLite local ni backends falsos.
 *  - **Origen local estable**: el IndexedDB queda atado al origen; el puerto se persiste y
 *    si cambia con datos ya escritos, se FALLA en vez de crear un universo nuevo en silencio.
 *  - **Perfil privado**: `~/.local/share/ovaultd/runtime` en 0700 (override: `OVAULTD_RUNTIME_DIR`).
 *
 * El puente con la página es JSON puro: `call(method, params)` → `dispatch` del runtime.
 */

import { chmodSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createServer, type Server } from 'node:http';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { chromium, type BrowserContext, type Page } from 'playwright';

export const RUNTIME_PORT_DEFAULT = 18_931;

export interface BrowserRuntimeOptions {
  /** Directorio del perfil (default: `~/.local/share/ovaultd/runtime`). */
  dataDir?: string;
  /** Puerto del origen local (default: `OVAULTD_RUNTIME_PORT` o 18931). */
  port?: number;
  /** Ruta al bundle del núcleo (default: `packages/core/dist/runtime.js`). */
  coreBundlePath?: string;
  headless?: boolean;
}

export interface RuntimeInfo {
  version: string;
  schemaVersion: number;
  origin: string;
  userAgent: string;
}

const PAGINA = `<!doctype html>
<html lang="es">
<head><meta charset="utf-8"><title>ovaultd runtime</title></head>
<body>
<script src="/runtime.js"></script>
</body>
</html>
`;

/** Ruta por defecto al bundle del núcleo, resolviendo desde este paquete. */
const bundlePorDefecto = (): string =>
  fileURLToPath(new URL('../../core/dist/runtime.js', import.meta.url));

export class BrowserRuntime {
  private secuencia = 0;

  private constructor(
    private readonly contexto: BrowserContext,
    private readonly pagina: Page,
    private readonly servidor: Server,
    readonly origin: string,
    readonly dataDir: string,
    readonly port: number,
  ) {}

  static async open(opciones: BrowserRuntimeOptions = {}): Promise<BrowserRuntime> {
    const dataDir =
      opciones.dataDir ??
      process.env.OVAULTD_RUNTIME_DIR ??
      join(homedir(), '.local', 'share', 'ovaultd', 'runtime');
    const port =
      opciones.port ??
      (process.env.OVAULTD_RUNTIME_PORT ? Number(process.env.OVAULTD_RUNTIME_PORT) : undefined) ??
      RUNTIME_PORT_DEFAULT;
    const bundle = opciones.coreBundlePath ?? bundlePorDefecto();

    if (!existsSync(bundle)) {
      throw new Error(
        `falta el bundle del núcleo: ${bundle}\n` +
          'compilalo con: npm run build -w @ovaultd/core',
      );
    }

    // Perfil e identidad del origen: 0700 y puerto persistido.
    mkdirSync(dataDir, { recursive: true, mode: 0o700 });
    chmodSync(dataDir, 0o700);

    const archivoOrigen = join(dataDir, 'origin.txt');
    const origin = `http://127.0.0.1:${port}`;
    if (existsSync(archivoOrigen)) {
      const anterior = readFileSync(archivoOrigen, 'utf8').trim();
      if (anterior !== origin) {
        throw new Error(
          `el origen cambió (${anterior} → ${origin}) y el IndexedDB está atado al anterior.\n` +
            `Usá el puerto ${anterior.split(':').pop()} o movés el perfil a mano — no se crea un estado nuevo en silencio.`,
        );
      }
    }

    const contenido = readFileSync(bundle);
    const servidor = createServer((req, res) => {
      if (req.url === '/runtime.js') {
        res.writeHead(200, { 'content-type': 'text/javascript; charset=utf-8' });
        res.end(contenido);
        return;
      }
      if (req.url === '/' || req.url === '/index.html') {
        res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
        res.end(PAGINA);
        return;
      }
      res.writeHead(404).end();
    });

    await new Promise<void>((resolve, reject) => {
      servidor.once('error', (error) => {
        reject(
          new Error(
            `no se pudo escuchar en ${origin}: ${String(error)}\n` +
              '(¿otro runtime del mismo device está abierto? Es a propósito: un solo dueño del perfil)',
          ),
        );
      });
      servidor.listen(port, '127.0.0.1', () => resolve());
    });

    // Perfil persistente: mismo userDataDir ⇒ mismo IndexedDB, siempre.
    const contexto = await chromium.launchPersistentContext(join(dataDir, 'chromium-profile'), {
      headless: opciones.headless ?? true,
      args: ['--no-first-run', '--no-default-browser-check'],
    });

    try {
      const pagina = contexto.pages()[0] ?? (await contexto.newPage());
      await pagina.goto(origin + '/');
      await pagina.waitForFunction(() => Boolean((globalThis as { ovaultdRuntime?: unknown }).ovaultdRuntime));

      writeFileSync(archivoOrigen, `${origin}\n`, { mode: 0o600 });
      chmodSync(archivoOrigen, 0o600);

      return new BrowserRuntime(contexto, pagina, servidor, origin, dataDir, port);
    } catch (error) {
      await contexto.close().catch(() => undefined);
      servidor.close();
      throw error;
    }
  }

  /** Alto nivel: una llamada al runtime de la página (JSON in/out). */
  async call<T = unknown>(method: string, params?: Record<string, unknown>): Promise<T> {
    const id = (this.secuencia += 1);
    const respuesta = (await this.pagina.evaluate(
      (req: { id: number; method: string; params?: Record<string, unknown> }) =>
        (globalThis as unknown as {
          ovaultdRuntime: { dispatch: (request: unknown) => Promise<unknown> };
        }).ovaultdRuntime.dispatch(req),
      { id, method, params },
    )) as { id: number; ok: boolean; result?: unknown; error?: { message: string; name?: string } };

    if (!respuesta || respuesta.id !== id) {
      throw new Error(`respuesta desincronizada del runtime (id ${id})`);
    }
    if (!respuesta.ok) {
      throw new Error(`runtime ${method}: ${respuesta.error?.message ?? 'error desconocido'}`);
    }
    return respuesta.result as T;
  }

  /** Info del runtime (versión, esquema, origen). */
  async info(): Promise<RuntimeInfo> {
    return this.call<RuntimeInfo>('runtime.info');
  }

  /** Cierra todo: contexto (flush de IndexedDB) y servidor. */
  async close(): Promise<void> {
    await this.contexto.close().catch(() => undefined);
    await new Promise<void>((resolve) => {
      this.servidor.close(() => resolve());
    });
  }
}

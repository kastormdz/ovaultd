/**
 * `BrowserRuntime` — el runtime durable del CLI: Chromium headless con perfil
 * persistente propio, sirviendo el bundle del núcleo desde un origen local estable.
 *
 * Decisiones (del plan de fase 2):
 *  - **Un solo motor durable**: IndexedDB vive acá; no hay SQLite local ni backends falsos.
 *  - **Origen local estable**: el IndexedDB queda atado al origen; el puerto se persiste y
 *    si cambia con datos ya escritos, se FALLA en vez de crear un universo nuevo en silencio.
 *  - **Perfil privado**: `~/.local/share/ovaultd/runtime` en 0700 (override: `OVAULTD_RUNTIME_DIR`).
 *  - **Puente de archivos**: si se pasa `vaultRoot`, el servidor local expone `/fs/<op>`
 *    contra un `NodeFileAdapter` **confinado al vault** (sin travesía, sin symlinks, solo
 *    en 127.0.0.1). El runtime nunca toca archivos por su cuenta.
 *  - **Failpoint real**: `crash()` mata el proceso de Chromium con SIGKILL (sin shutdown
 *    limpio) para probar que lo confirmado sobrevive y lo a medias se recupera.
 *
 * El puente con la página es JSON puro: `call(method, params)` → `dispatch` del runtime.
 */

import {
  chmodSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  writeFileSync,
} from 'node:fs';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { chromium, type BrowserContext, type Page } from 'playwright';

import { NodeFileAdapter, VaultBoundaryError } from './NodeFileAdapter.js';

export const RUNTIME_PORT_DEFAULT = 18_931;

/**
 * `proxyFetch` existe para que la página no pague un preflight CORS por blob, pero un proxy
 * abierto es SSRF local: cualquier proceso puede alcanzar servicios internos y reenviar
 * credenciales. Solo se permite el MISMO ORIGEN que el server de la sesión; sin `serverUrl`
 * el proxy queda cerrado (fail closed).
 */
export function destinoPermitido(destino: string, serverUrl: string | undefined): boolean {
  if (!serverUrl) return false;
  try {
    const d = new URL(destino);
    const s = new URL(serverUrl);
    return (d.protocol === 'http:' || d.protocol === 'https:') && d.origin === s.origin;
  } catch {
    return false;
  }
}

/** Headers que el proxy NO reenvía: los de control del puente y los de conexión. */
export function sanearHeaders(headers: Record<string, string>): Record<string, string> {
  const prohibidos = new Set(['cookie', 'host', 'connection', 'transfer-encoding', 'content-length']);
  const salida: Record<string, string> = {};
  for (const [clave, valor] of Object.entries(headers)) {
    const k = clave.toLowerCase();
    if (k.startsWith('x-ovaultd-') || prohibidos.has(k)) continue;
    salida[clave] = valor;
  }
  return salida;
}

export interface BrowserRuntimeOptions {
  /** Directorio del perfil (default: `~/.local/share/ovaultd/runtime`). */
  dataDir?: string;
  /** Puerto del origen local (default: `OVAULTD_RUNTIME_PORT` o 18931). */
  port?: number;
  /** Ruta al bundle del núcleo (default: `packages/core/dist/runtime.js`). */
  coreBundlePath?: string;
  headless?: boolean;
  /** Si se define, habilita el puente `/fs/*` confinado a esta raíz (el vault). */
  vaultRoot?: string;
  /** URL base del server de la sesión: es el ÚNICO destino al que `proxyFetch` reenvía. */
  serverUrl?: string;
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

// --------------------------------------------------------------------- puente /fs

const LIMITE_CUERPO = 64 * 1024 * 1024; // 32 MiB de contenido + base64 + sobre, con aire

function responder(res: ServerResponse, cuerpo: unknown): void {
  const texto = JSON.stringify(cuerpo);
  res.writeHead(200, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(texto),
  });
  res.end(texto);
}

function ejecutarOp(
  vault: NodeFileAdapter,
  op: string,
  p: Record<string, unknown>,
  serverUrl: string | undefined,
): unknown {
  switch (op) {
    case 'stat':
      return vault.stat(String(p.path));
    case 'statMany': {
      // Stat masivo en UN request: el escaneo del CLI dejaba de pagar 1000 round-trips.
      const rutas = Array.isArray(p.paths) ? (p.paths as string[]) : [];
      return { stats: rutas.map((ruta) => vault.stat(ruta)) };
    }
    case 'read':
      return { b64: Buffer.from(vault.read(String(p.path))).toString('base64') };
    case 'readMany':
      // Una tanda de lecturas en UN round-trip: el escaneo inicial ya no paga el bridge
      // por archivo (el cuello que quedaba en T1). El límite de cuerpo del bridge cubre
      // lotes de hasta 100 notas de 100 KB.
      return {
        blobs: (p.paths as string[]).map((ruta) => {
          try {
            return Buffer.from(vault.read(ruta)).toString('base64');
          } catch {
            return null;
          }
        }),
      };
    // Trace de diagnóstico desde la página (el console del navegador no llega al CLI).
    case 'log':
      console.error(`[runtime] ${String(p.msg)}`);
      return {};
    // Proxy HTTP desde la página: el PUT de blobs va por acá para NO pagar el preflight
    // CORS del navegador (uno por URL, y cada blob es una URL). node lo reenvía al server.
    // IIFE async: `ejecutarOp` es síncrona y el call site resuelve la promesa.
    case 'proxyFetch':
      return (async () => {
        const peticion = p as {
          url: string;
          method: string;
          headers: Record<string, string>;
          b64: string | null;
        };
        if (!destinoPermitido(peticion.url, serverUrl)) {
          throw new VaultBoundaryError(
            'proxyFetch solo puede hablar con el server de la sesión (destino rechazado)',
          );
        }
        const cuerpo = peticion.b64 ? Buffer.from(peticion.b64, 'base64') : undefined;
        const t0 = Date.now();
        const respuesta = await fetch(peticion.url, {
          method: peticion.method,
          headers: sanearHeaders(peticion.headers),
          body: cuerpo,
        });
        const bytes = Buffer.from(await respuesta.arrayBuffer());
        console.error(
          `[proxy] ${peticion.method} ${Math.round((cuerpo?.length ?? 0) / 1024)}KB ${Date.now() - t0}ms`,
        );
        return { status: respuesta.status, b64: bytes.length ? bytes.toString('base64') : null };
      })();
    case 'writeTemp':
      return vault.writeTemp(String(p.dir), new Uint8Array(Buffer.from(String(p.b64), 'base64')));
    case 'rename':
      return (vault.rename(String(p.from), String(p.to)), { ok: true });
    case 'unlink':
      return (vault.unlink(String(p.path)), { ok: true });
    case 'mkdirp':
      return (vault.mkdirp(String(p.path)), { ok: true });
    case 'list':
      return vault.list(String(p.path));
    default:
      throw new VaultBoundaryError(`operación desconocida: ${op}`);
  }
}

function manejarFs(
  req: IncomingMessage,
  res: ServerResponse,
  vault: NodeFileAdapter,
  serverUrl: string | undefined,
): void {
  const op = req.url!.slice('/fs/'.length);
  const partes: Buffer[] = [];
  let total = 0;
  let excedido = false;
  req.on('data', (c: Buffer) => {
    total += c.length;
    if (total > LIMITE_CUERPO) {
      excedido = true;
      req.destroy();
      return;
    }
    partes.push(c);
  });
  req.on('end', () => {
    if (excedido) {
      responder(res, { ok: false, error: { message: 'cuerpo demasiado grande' } });
      return;
    }
    // Proxy binario ANTES del parse de JSON: el body son bytes crudos y no parsea.
    if (op === 'proxyFetch') {
      void (async () => {
        try {
          const destino = decodeURIComponent(String(req.headers['x-ovaultd-url'] ?? ''));
          const metodo = String(req.headers['x-ovaultd-method'] ?? 'PUT');
          if (!destinoPermitido(destino, serverUrl)) {
            responder(res, {
              ok: false,
              error: {
                message: 'proxyFetch solo puede hablar con el server de la sesión (destino rechazado)',
                name: 'VaultBoundaryError',
              },
            });
            return;
          }
          let headers: Record<string, string> = {};
          try {
            headers = JSON.parse(String(req.headers['x-ovaultd-headers'] ?? '{}'));
          } catch {
            headers = {};
          }
          const crudo = Buffer.concat(partes);
          const respuesta = await fetch(destino, {
            method: metodo,
            headers: sanearHeaders(headers),
            body: crudo.length ? crudo : undefined,
          });
          const bytes = Buffer.from(await respuesta.arrayBuffer());
          res.writeHead(200, {
            'content-type': 'application/octet-stream',
            'content-length': String(bytes.length),
            'x-ovaultd-status': String(respuesta.status),
          });
          res.end(bytes);
        } catch (error) {
          const err = error as Error;
          console.error(`[proxyFetch] fallo: ${err.message}`);
          responder(res, { ok: false, error: { message: err.message, name: err.name } });
        }
      })();
      return;
    }
    let params: Record<string, unknown>;
    try {
      params = JSON.parse(Buffer.concat(partes).toString('utf8') || '{}') as Record<string, unknown>;
    } catch {
      responder(res, { ok: false, error: { message: 'JSON inválido' } });
      return;
    }
    try {
      // Proxy binario: la página manda los bytes crudos y los metadatos en headers; acá
      // se reenvían con node (evita el preflight CORS por URL y el base64 de 180 KB).
      const resultado = ejecutarOp(vault, op, params, serverUrl);
      // `proxyFetch` devuelve una promesa (el HTTP real corre en node, fuera del bridge).
      if (resultado instanceof Promise) {
        resultado.then(
          (valor) => responder(res, { ok: true, result: valor }),
          (error) => {
            const err = error as Error;
            responder(res, { ok: false, error: { message: err.message, name: err.name } });
          },
        );
        return;
      }
      responder(res, { ok: true, result: resultado });
    } catch (error) {
      const err = error as Error;
      responder(res, { ok: false, error: { message: err.message, name: err.name } });
    }
  });
}

// --------------------------------------------------------------------- runtime

export class BrowserRuntime {
  private secuencia = 0;

  private constructor(
    private readonly contexto: BrowserContext,
    private readonly pagina: Page,
    private readonly servidor: Server,
    readonly origin: string,
    readonly dataDir: string,
    readonly port: number,
    /** Puente de archivos confinado al vault (si se abrió con `vaultRoot`). */
    readonly vault: NodeFileAdapter | null,
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
    const vault = opciones.vaultRoot ? new NodeFileAdapter(opciones.vaultRoot) : null;
    const serverUrl = opciones.serverUrl;

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
      if (vault && req.method === 'POST' && req.url?.startsWith('/fs/')) {
        manejarFs(req, res, vault, serverUrl);
        return;
      }
      res.writeHead(404).end();
    });

    await new Promise<void>((resolve, reject) => {
      servidor.once('error', (error) => {
        const ocupado = (error as NodeJS.ErrnoException).code === 'EADDRINUSE';
        reject(
          new Error(
            `no se pudo escuchar en ${origin}: ${String(error)}\n` +
              (ocupado
                ? 'el runtime del vault ya está abierto por otro proceso (el servicio de sync o otro `ovaultd`).\n' +
                  'Es a propósito: un solo dueño del perfil. Con el servicio activo el vault YA se sincroniza\n' +
                  'cada pocos segundos: no hace falta un sync manual. Para uno puntual:\n' +
                  '  systemctl --user stop ovaultd-sync && ovaultd sync ; systemctl --user start ovaultd-sync'
                : '(otro runtime del mismo device está abierto: un solo dueño del perfil)'),
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

      return new BrowserRuntime(contexto, pagina, servidor, origin, dataDir, port, vault);
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

  /**
   * Failpoint REAL: mata el proceso de Chromium con SIGKILL (sin flush ni cierre limpio)
   * y libera el puerto para poder reabrir el mismo perfil. Lo que estaba confirmado
   * sobrevive; lo a medias queda para la recuperación.
   */
  async crash(): Promise<number> {
    const pid = this.pidChromium();
    process.kill(pid, 'SIGKILL');
    await new Promise<void>((resolve) => this.servidor.close(() => resolve()));
    return pid;
  }

  private pidChromium(): number {
    // 1) Chrome completo deja `SingletonLock` como symlink `<hostname>-<pid>`.
    try {
      const destino = readlinkSync(join(this.dataDir, 'chromium-profile', 'SingletonLock'));
      const pid = Number(destino.slice(destino.lastIndexOf('-') + 1));
      if (Number.isInteger(pid) && pid > 0) return pid;
    } catch {
      /* el headless shell no crea SingletonLock: se busca por /proc */
    }
    // 2) Proceso principal de Chromium por `--user-data-dir` en /proc (Linux).
    const perfil = join(this.dataDir, 'chromium-profile');
    try {
      for (const entrada of readdirSync('/proc')) {
        if (!/^\d+$/.test(entrada)) continue;
        try {
          const cmdline = readFileSync(`/proc/${entrada}/cmdline`, 'utf8');
          if (cmdline.includes(`--user-data-dir=${perfil}`) && !cmdline.includes('--type=')) {
            return Number(entrada);
          }
        } catch {
          /* proceso murió o sin permiso: siguiente */
        }
      }
    } catch {
      /* sin /proc */
    }
    // 3) Último recurso: la vía de playwright (null en contextos persistentes).
    const proceso = (
      this.contexto as unknown as { browser?: () => { process?: () => { pid?: number } | null } | null }
    ).browser?.()?.process?.();
    if (proceso?.pid && proceso.pid > 0) return proceso.pid;
    throw new Error('no pude determinar el PID de Chromium para el failpoint');
  }

  /** Cierra todo: contexto (flush de IndexedDB) y servidor. */
  async close(): Promise<void> {
    await this.contexto.close().catch(() => undefined);
    await new Promise<void>((resolve) => {
      this.servidor.close(() => resolve());
    });
  }
}

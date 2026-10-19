/**
 * Cliente HTTP del wire v2 — la superficie que el server expone hoy.
 *
 * Nada de esto descifra: los secretos (vault_key, KEK) viven en el llamador.
 * El error del wire (`{t:"error", code, detail}`) se mapea a `ApiError`.
 */

import type {
  AccessResponse,
  ChallengeResponse,
  ChangesWire,
  CreateVaultWire,
  MeResponse,
  SigninResponse,
  VaultMaterial,
  VaultSummary,
} from '@ovaultd/protocol';
import { b64Encode } from '@ovaultd/protocol';

export class ApiError extends Error {
  readonly status: number;
  readonly code: string | null;
  readonly detail: string | null;

  constructor(status: number, code: string | null, detail: string | null, message?: string) {
    super(message ?? `HTTP ${status}${code ? ` (${code})` : ''}${detail ? `: ${detail}` : ''}`);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.detail = detail;
  }
}

/** Normaliza el server: agrega esquema si falta y saca slashes finales. */
export const normalizeServer = (servidor: string): string => {
  let out = servidor.trim();
  if (!/^https?:\/\//i.test(out)) out = `http://${out}`;
  return out.replace(/\/+$/, '');
};

/** Mapea un cuerpo de error del wire a `ApiError` (sin romper si no es JSON). */
export const parseErrorBody = (status: number, cuerpo: string): ApiError => {
  try {
    const valor = JSON.parse(cuerpo) as { t?: string; code?: string; detail?: string };
    if (typeof valor === 'object' && valor !== null && typeof valor.code === 'string') {
      return new ApiError(status, valor.code, typeof valor.detail === 'string' ? valor.detail : null);
    }
  } catch {
    /* no era JSON */
  }
  return new ApiError(status, null, null);
};

export class OvaultdClient {
  private readonly base: string;

  constructor(
    servidor: string,
    private token: string | null = null,
    private readonly fetchImpl: typeof fetch = globalThis.fetch,
  ) {
    this.base = normalizeServer(servidor);
  }

  setToken(token: string | null): void {
    this.token = token;
  }

  private async pedir(
    metodo: string,
    ruta: string,
    cuerpo?: string | Uint8Array,
    extraHeaders: Record<string, string> = {},
  ): Promise<Response> {
    const headers: Record<string, string> = { ...extraHeaders };
    if (this.token) headers.authorization = `Bearer ${this.token}`;

    let respuesta: Response;
    try {
      respuesta = await this.fetchImpl(`${this.base}${ruta}`, {
        method: metodo,
        headers,
        body: cuerpo,
        // Sin timeout, un borde que acepta la conexión y no responde deja el CLI colgado
        // para siempre (síntoma: "no hace nada"). 30 s es de sobra para el API.
        signal: AbortSignal.timeout(30_000),
      });
    } catch (error) {
      const abortado = (error as { name?: string }).name === 'TimeoutError';
      throw new ApiError(
        0,
        'network',
        null,
        abortado ? `el server no respondió en 30 s: ${this.base}` : `no se pudo conectar a ${this.base}: ${String(error)}`,
      );
    }

    if (!respuesta.ok) {
      const texto = await respuesta.text().catch(() => '');
      throw parseErrorBody(respuesta.status, texto);
    }
    return respuesta;
  }

  private async pedirJson<T>(metodo: string, ruta: string, cuerpo?: unknown): Promise<T> {
    const respuesta = await this.pedir(metodo, ruta, cuerpo === undefined ? undefined : JSON.stringify(cuerpo), {
      'content-type': 'application/json',
    });
    return (await respuesta.json()) as T;
  }

  healthz(): Promise<boolean> {
    return this.fetchImpl(`${this.base}/healthz`, { method: 'GET' })
      .then((respuesta) => respuesta.ok)
      .catch(() => false);
  }

  signin(email: string, password: string, name?: string, platform?: string): Promise<SigninResponse> {
    return this.pedirJson<SigninResponse>('POST', '/api/v1/auth/signin', {
      email,
      password,
      ...(name ? { name } : {}),
      ...(platform ? { platform } : {}),
    });
  }

  signout(): Promise<void> {
    return this.pedir('POST', '/api/v1/auth/signout').then(() => undefined);
  }

  me(): Promise<MeResponse> {
    return this.pedirJson<MeResponse>('GET', '/api/v1/me');
  }

  listVaults(): Promise<VaultSummary[]> {
    return this.pedirJson<VaultSummary[]>('GET', '/api/v1/vaults');
  }

  createVault(body: CreateVaultWire): Promise<{ id: number }> {
    return this.pedirJson<{ id: number }>('POST', '/api/v1/vaults', body);
  }

  material(vault: number): Promise<VaultMaterial> {
    return this.pedirJson<VaultMaterial>('GET', `/api/v1/vaults/${vault}/material`);
  }

  challenge(vault: number): Promise<ChallengeResponse> {
    return this.pedirJson<ChallengeResponse>('GET', `/api/v1/vaults/${vault}/challenge`);
  }

  access(vault: number, proof: Uint8Array): Promise<AccessResponse> {
    return this.pedirJson<AccessResponse>('POST', `/api/v1/vaults/${vault}/access`, {
      proof: b64Encode(proof),
    });
  }

  changes(vault: number, since: number, limit = 100): Promise<ChangesWire> {
    return this.pedirJson<ChangesWire>('GET', `/api/v1/vaults/${vault}/changes?since=${since}&limit=${limit}`);
  }

  async putBlob(vault: number, blobIdHex: string, bytes: Uint8Array): Promise<void> {
    await this.pedir('PUT', `/api/v1/vaults/${vault}/blobs/${blobIdHex}`, bytes, {
      'content-type': 'application/octet-stream',
    });
  }

  async getBlob(vault: number, blobIdHex: string): Promise<Uint8Array> {
    const respuesta = await this.pedir('GET', `/api/v1/vaults/${vault}/blobs/${blobIdHex}`);
    return new Uint8Array(await respuesta.arrayBuffer());
  }
}

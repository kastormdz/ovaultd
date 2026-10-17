/**
 * Cliente HTTP: normalización del server, mapeo del sobre de error y un
 * roundtrip con un `fetch` doble (sin red).
 */

import { describe, expect, it, vi } from 'vitest';

import { ApiError, OvaultdClient, normalizeServer, parseErrorBody } from '../src/api.js';

describe('normalizeServer', () => {
  it('agrega esquema y saca slashes finales', () => {
    expect(normalizeServer('127.0.0.1:8791')).toBe('http://127.0.0.1:8791');
    expect(normalizeServer('http://localhost:8888/')).toBe('http://localhost:8888');
    expect(normalizeServer(' https://vault.example ')).toBe('https://vault.example');
  });
});

describe('parseErrorBody', () => {
  it('mapea el sobre del wire', () => {
    const error = parseErrorBody(409, '{"t":"error","code":"bad_request","detail":"ese vault_id ya está registrado"}');
    expect(error).toBeInstanceOf(ApiError);
    expect(error.status).toBe(409);
    expect(error.code).toBe('bad_request');
    expect(error.detail).toContain('ya está registrado');
  });

  it('no rompe con cuerpos que no son JSON', () => {
    const error = parseErrorBody(502, '<html>bad gateway</html>');
    expect(error.code).toBeNull();
    expect(error.status).toBe(502);
  });
});

describe('OvaultdClient', () => {
  it('habla signin y ajusta el token para las siguientes llamadas', async () => {
    const llamadas: Array<{ url: string; headers: Record<string, string>; body?: string }> = [];
    const fetchDoble = vi.fn(async (url: string | URL, init?: RequestInit) => {
      llamadas.push({
        url: String(url),
        headers: (init?.headers ?? {}) as Record<string, string>,
        body: typeof init?.body === 'string' ? init.body : undefined,
      });
      if (String(url).endsWith('/auth/signin')) {
        return new Response(JSON.stringify({ token: 'tok-123', device_id: 9 }), { status: 200 });
      }
      return new Response(JSON.stringify({ account_id: 1, email: 'a@example.test', devices: [] }), {
        status: 200,
      });
    }) as unknown as typeof fetch;

    const cliente = new OvaultdClient('127.0.0.1:8791', null, fetchDoble);
    const sesion = await cliente.signin('a@example.test', 'pw');
    cliente.setToken(sesion.token);
    const me = await cliente.me();

    expect(sesion).toEqual({ token: 'tok-123', device_id: 9 });
    expect(me.account_id).toBe(1);
    expect(llamadas[0]!.url).toBe('http://127.0.0.1:8791/api/v1/auth/signin');
    expect(llamadas[1]!.headers.authorization).toBe('Bearer tok-123');
  });

  it('tira ApiError con el código del server en respuestas no-2xx', async () => {
    const fetchDoble = vi.fn(async () =>
      new Response('{"t":"error","code":"unauthorized","detail":"token vencido"}', { status: 401 }),
    ) as unknown as typeof fetch;

    const cliente = new OvaultdClient('127.0.0.1:8791', 'viejo', fetchDoble);
    await expect(cliente.me()).rejects.toMatchObject({ status: 401, code: 'unauthorized' });
  });
});

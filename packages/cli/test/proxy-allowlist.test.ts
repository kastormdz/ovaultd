import { describe, expect, it } from 'vitest';

import { destinoPermitido, sanearHeaders } from '../src/browser-runtime.js';

describe('allowlist de proxyFetch (F-01)', () => {
  const server = 'https://obs.example.com';

  it('permite el mismo origen que el server de la sesión', () => {
    expect(destinoPermitido('https://obs.example.com/api/v1/blobs/abc', server)).toBe(true);
  });

  it('rechaza otro host (SSRF local)', () => {
    expect(destinoPermitido('http://127.0.0.1:9999/metabolismo', server)).toBe(false);
    expect(destinoPermitido('http://169.254.169.254/latest/meta-data', server)).toBe(false);
  });

  it('rechaza el mismo host en otro puerto (origen distinto)', () => {
    expect(destinoPermitido('https://obs.example.com:8443/api', server)).toBe(false);
  });

  it('rechaza esquemas que no son http(s)', () => {
    expect(destinoPermitido('file:///etc/passwd', server)).toBe(false);
    expect(destinoPermitido('gopher://127.0.0.1:6379/_INFO', server)).toBe(false);
    expect(destinoPermitido('no-es-una-url', server)).toBe(false);
  });

  it('sin serverUrl el proxy queda cerrado (fail closed)', () => {
    expect(destinoPermitido('https://obs.example.com/api', undefined)).toBe(false);
  });
});

describe('saneo de headers del proxy (F-01)', () => {
  it('no reenvía los headers de control del puente ni cookie/host', () => {
    const salida = sanearHeaders({
      'x-ovaultd-url': 'http://evil',
      'X-OVaultd-Method': 'GET',
      cookie: 'sesion=abc',
      host: 'evil',
      'content-length': '10',
      authorization: 'Bearer token-real',
      'content-type': 'application/octet-stream',
    });
    expect(salida).toEqual({
      authorization: 'Bearer token-real',
      'content-type': 'application/octet-stream',
    });
  });
});

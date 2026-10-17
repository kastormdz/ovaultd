/**
 * probe-o1.mjs — mide la pasada 1 (nonce) de `sync2` en AISLAMIENTO.
 *
 * Por qué existe: `streaming.mjs` genera su archivo de prueba con un buffer de 64 MiB que queda residual en el
 * allocator, así que su `PEAK_RSS_PASS1` sale contaminado (~81 MiB con 64 MiB de archivo). Acá la medición arranca
 * de un proceso limpio y se repite con **tres tamaños distintos**: si el pico NO escala con el tamaño, la pasada 1
 * es O(1). Ese es el número que sostiene la decisión H-3 de `docs/crypto-gate.md`.
 *
 * Uso:  node probe-o1.mjs            (8, 64 y 256 MiB)
 *       node probe-o1.mjs 1024       (tamaño máximo en MiB a probar)
 *
 * Nunca escribe dentro del repo (override con PROBE_DIR para elegir el directorio).
 */
import { createReadStream, openSync, writeSync, closeSync, unlinkSync, mkdirSync, existsSync } from 'node:fs';
import { createHmac } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const DIR = process.env.PROBE_DIR ?? join(tmpdir(), 'ovaultd-spike-stream');
const MAX_MIB = Number(process.argv[2] ?? 256);
const SIZES = [8, 64, 256].filter((m) => m <= MAX_MIB);
const CHUNK = 65536;
const KEY = Buffer.alloc(32, 7);

// Intento con la misma librería del plugin (@noble); si el specifier cambió, mido con node:crypto.
let nobleHmac = null;
try {
  const m = await import('@noble/hashes/hmac');
  const s = await import('@noble/hashes/sha2');
  nobleHmac = { hmac: m.hmac, sha256: s.sha256 };
} catch {
  nobleHmac = null;
}

if (!existsSync(DIR)) mkdirSync(DIR, { recursive: true });

async function medir(path, lib) {
  const base = process.memoryUsage().rss;
  let peak = base;
  const h = lib === 'noble' ? nobleHmac.hmac.create(nobleHmac.sha256, new Uint8Array(KEY)) : createHmac('sha256', KEY);
  for await (const c of createReadStream(path, { highWaterMark: CHUNK })) {
    h.update(c);
    const rss = process.memoryUsage().rss;
    if (rss > peak) peak = rss;
  }
  const d = Buffer.from(h.digest());
  return { base, peak, first8: d.subarray(0, 8).toString('hex') };
}

function crear(path, mib) {
  const fd = openSync(path, 'w');
  const chunk = Buffer.alloc(CHUNK, 0xab);
  for (let i = 0; i < (mib * 1048576) / CHUNK; i++) writeSync(fd, chunk);
  closeSync(fd);
}

const mib = (v) => (v / 1048576).toFixed(2);

console.log('size_MiB\tlib\tbase_MiB\tpeak_MiB\tdelta_MiB\thmac8');
const deltas = [];
for (const size of SIZES) {
  const f = `${DIR}/f${size}.bin`;
  try {
    crear(f, size);
    for (const lib of nobleHmac ? ['node', 'noble'] : ['node']) {
      const r = await medir(f, lib);
      deltas.push({ size, lib, delta: r.peak - r.base });
      console.log(`${size}\t${lib}\t${mib(r.base)}\t${mib(r.peak)}\t${mib(r.peak - r.base)}\t${r.first8}`);
    }
  } finally {
    if (existsSync(f)) unlinkSync(f);
  }
}

const porLib = new Map();
for (const d of deltas) porLib.set(d.lib, [...(porLib.get(d.lib) ?? []), d]);
let ok = true;
for (const [lib, ds] of porLib) {
  const max = Math.max(...ds.map((d) => d.delta));
  const min = Math.min(...ds.map((d) => d.delta));
  const escala = max > min * 2 + 4 * 1048576; // ¿el pico crece con el archivo?
  if (escala) ok = false;
  console.log(`\n${lib}: delta entre ${mib(min)} y ${mib(max)} MiB sobre ${ds.length} tamanos -> ${escala ? 'ESCALA (NO es O(1))' : 'NO escala (O(1))'}`);
}
console.log(ok ? '\nVEREDICTO: pasada 1 es O(1) en memoria.' : '\nVEREDICTO: la pasada 1 ESCALA con el tamanio: revisar.');
process.exit(ok ? 0 : 1);

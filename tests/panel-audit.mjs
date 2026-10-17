// Gate de W3 — auditoría del panel ovaultd: status, consola/CSP, desbordes y capturas.
//
// Uso: node panel-audit.mjs   (desde ~/Cloud/repos/github/ovaultd/tests)
//   PANEL_BASE      base del server    (default http://127.0.0.1:8894)
//   PANEL_EMAIL     cuenta admin del lab
//   PANEL_PASSWORD  su password
//   PANEL_OUT       carpeta de capturas (default ~/tmp/panel-shot/audit)
//
// Credenciales SIEMPRE por entorno: el repo es público y las del lab no se versionan.
//
// Sale con código 0 solo si TODAS las páginas pasan: status 200, cero errores de
// consola (las violaciones de CSP se reportan ahí), cero requests fallidos y cero
// desborde horizontal — tanto en escritorio como en móvil.
import { chromium } from '@playwright/test';
import { mkdirSync } from 'node:fs';

const B = process.env.PANEL_BASE ?? 'http://127.0.0.1:8894';
const EMAIL = process.env.PANEL_EMAIL;
const PASSWORD = process.env.PANEL_PASSWORD;
const OUT = process.env.PANEL_OUT ?? process.env.HOME + '/tmp/panel-shot/audit';

if (!EMAIL || !PASSWORD) {
  console.error('Faltan PANEL_EMAIL y PANEL_PASSWORD (las credenciales del lab no viven en el repo).');
  process.exit(2);
}

const PAGINAS = [
  ['dashboard', '/admin'],
  ['cuentas', '/admin/cuentas'],
  ['vaults', '/admin/vaults'],
  ['vault-detalle', '/admin/vaults/detalle?id=1'],
  ['devices', '/admin/devices'],
  ['auditoria', '/admin/auditoria'],
  ['ajustes', '/admin/ajustes'],
  ['vault-borrar', '/admin/vaults/borrar?id=1'],
  ['cuenta-borrar', '/admin/cuentas/borrar?id=3'],
  ['login', null], // sin sesión: se audita aparte, en su propio contexto
];

const VIEWPORTS = [
  { nombre: 'escritorio', width: 1440, height: 900, dsf: 1.5 },
  { nombre: 'movil', width: 390, height: 844, dsf: 2 },
];

mkdirSync(OUT, { recursive: true });
const browser = await chromium.launch();
const problemas = [];
const resumen = [];

async function auditarViewport(vp) {
  const ctx = await browser.newContext({
    viewport: { width: vp.width, height: vp.height },
    deviceScaleFactor: vp.dsf,
  });
  const page = await ctx.newPage();

  // La consola y los requests fallidos se acumulan por página.
  let consola = [];
  let fallidos = [];
  page.on('console', (m) => {
    if (m.type() === 'error') consola.push(m.text().slice(0, 200));
  });
  page.on('pageerror', (e) => consola.push(`pageerror: ${String(e).slice(0, 200)}`));
  page.on('requestfailed', (r) => fallidos.push(`${r.method()} ${r.url().slice(0, 120)}`));
  page.on('response', (r) => {
    if (r.status() >= 400) fallidos.push(`${r.status()} ${r.url().slice(0, 120)}`);
  });

  // ── login por el formulario real ──
  await page.goto(`${B}/admin/login`, { waitUntil: 'domcontentloaded' });
  await page.fill('input[name="email"]', EMAIL);
  await page.fill('input[name="password"]', PASSWORD);
  await Promise.all([page.waitForURL('**/admin', { timeout: 10000 }), page.click('button[type="submit"]')]);

  for (const [nombre, ruta] of PAGINAS) {
    if (ruta === null) continue;
    consola = [];
    fallidos = [];
    const r = await page.goto(`${B}${ruta}`, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(200);

    const m = await page.evaluate(() => {
      // Un desborde con scroll propio (overflow-x auto/scroll en algún ancestro) es
      // scroll intencional, no un defecto: la tarjeta con una tabla ancha se desliza.
      const tieneScroll = (el) => {
        for (let n = el; n && n !== document.body; n = n.parentElement) {
          const ox = getComputedStyle(n).overflowX;
          if (ox === 'auto' || ox === 'scroll') return true;
        }
        return false;
      };
      const desbordan = [...document.querySelectorAll('*')]
        .filter((x) => x.scrollWidth > x.clientWidth + 3 && !tieneScroll(x))
        .map((x) => `${x.tagName}.${(x.className || '').toString().trim().slice(0, 40)}`)
        .slice(0, 5);
      return {
        titulo: document.title,
        h1: document.querySelector('h1, .hero .veredicto, .hero h1')?.textContent?.trim().slice(0, 60) ?? null,
        overflowsX: document.documentElement.scrollWidth > window.innerWidth + 3,
        anchoDoc: document.documentElement.scrollWidth,
        desbordan,
      };
    });

    const fila = { vp: vp.nombre, pagina: nombre, status: r?.status() ?? null, ...m, consola, fallidos };
    resumen.push(fila);

    if (fila.status !== 200) problemas.push(`${vp.nombre}/${nombre}: status ${fila.status}`);
    if (fila.overflowsX) problemas.push(`${vp.nombre}/${nombre}: desborde horizontal (doc ${fila.anchoDoc}px)`);
    if (m.desbordan.length) problemas.push(`${vp.nombre}/${nombre}: desbordan → ${m.desbordan.join(', ')}`);
    if (consola.length) problemas.push(`${vp.nombre}/${nombre}: consola → ${consola[0]}`);
    if (fallidos.length) problemas.push(`${vp.nombre}/${nombre}: request → ${fallidos[0]}`);

    if (vp.nombre === 'escritorio') {
      await page.screenshot({ path: `${OUT}/${nombre}.png`, fullPage: true });
    } else {
      await page.screenshot({ path: `${OUT}/movil-${nombre}.png`, fullPage: true });
    }
  }
  await ctx.close();
}

// ── la pantalla de login se audita SIN sesión (contexto limpio) ──
async function auditarLogin() {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1.5 });
  const page = await ctx.newPage();
  const consola = [];
  const fallidos = [];
  page.on('console', (m) => m.type() === 'error' && consola.push(m.text().slice(0, 200)));
  page.on('requestfailed', (r) => fallidos.push(`${r.method()} ${r.url().slice(0, 120)}`));
  page.on('response', (r) => r.status() >= 400 && fallidos.push(`${r.status()} ${r.url().slice(0, 120)}`));
  const r = await page.goto(`${B}/admin/login`, { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(200);
  await page.screenshot({ path: `${OUT}/login.png`, fullPage: true });
  const fila = { vp: 'escritorio', pagina: 'login', status: r?.status() ?? null, consola, fallidos };
  resumen.push(fila);
  if (fila.status !== 200) problemas.push(`login: status ${fila.status}`);
  if (consola.length) problemas.push(`login: consola → ${consola[0]}`);
  await ctx.close();
}

for (const vp of VIEWPORTS) await auditarViewport(vp);
await auditarLogin();
await browser.close();

console.log(JSON.stringify(resumen.map(({ pagina, vp, status, overflowsX, desbordan }) => ({ vp, pagina, status, overflowsX, desbordan })), null, 1));
if (problemas.length) {
  console.error(`\n✗ ${problemas.length} problema(s):`);
  for (const p of problemas) console.error(`  - ${p}`);
  process.exit(1);
}
console.log('\n✓ Panel OK: sin desbordes, sin errores de consola, sin requests fallidos.');

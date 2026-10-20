# Tema activo portable (`appearance.json` por campo) — Plan de implementación

**Goal:** que el **tema activo** y los **snippets habilitados** viajen entre dispositivos, sin
que el **modo claro/oscuro** (que Obsidian reescribe solo) genere colisiones ni revisiones.

**Arquitectura:** `appearance.json` entra en la lista blanca del filtro, pero con dos reglas
propias: (1) el scan lo hashea por **proyección portable** (solo `cssTheme` +
`enabledCssSnippets`), así un cambio de modo no se ve como cambio; (2) el merge 3-vías usa un
**merge por campo** — los campos portables los decide el remoto, el resto queda local. No se
toca el protocolo ni el wire: viaja el archivo entero como cualquier otro JSON.

**Tech Stack:** TypeScript (packages/core), vitest, el runtime del plugin/CLI.

**Decisiones (cerradas acá):**
- Campos portables: `cssTheme` (string) y `enabledCssSnippets` (array de strings, se ordena).
- Campos locales (nunca se aplican del remoto): `theme` (modo), `baseFontSize`,
  `interfaceFontFamily`, `textFontFamily`, `monospaceFontFamily`, `showViewHeader`,
  `nativeMenus`, y cualquier clave desconocida (se conserva la local).
- Conflicto del mismo campo portable: **gana el remoto** (es config compartida; el local
  puede volver a cambiarlo y se propaga).
- JSON inválido en cualquiera de los tres lados: **gana el local** (no se rompe nada).

---

## Task 1: Módulo `apariencia.ts` con la proyección y el merge

**Objective:** una única fuente de verdad de qué campos viajan y cómo se fusionan.

**Files:**
- Create: `packages/core/src/apariencia.ts`
- Test: `packages/core/test/apariencia.test.ts`

**Step 1: Escribir el test (falla)**

```ts
// packages/core/test/apariencia.test.ts
import { describe, expect, it } from 'vitest';
import { mergeDeApariencia, proyeccionDeApariencia, esApariencia } from '../src/apariencia.js';

const j = (o: object) => JSON.stringify(o, null, 2) + '\n';

describe('apariencia', () => {
  it('detecta el archivo por nombre', () => {
    expect(esApariencia('.obsidian/appearance.json')).toBe(true);
    expect(esApariencia('notas/appearance.json')).toBe(true);
    expect(esApariencia('.obsidian/community-plugins.json')).toBe(false);
  });

  it('la proyección solo mira los campos portables y ordena snippets', () => {
    const a = proyeccionDeApariencia(j({ cssTheme: 'Anu', theme: 'obsidian', enabledCssSnippets: ['b', 'a'] }));
    const b = proyeccionDeApariencia(j({ cssTheme: 'Anu', theme: 'moonstone', enabledCssSnippets: ['a', 'b'] }));
    expect(a).toBe(b); // el modo y el orden no cuentan
    expect(proyeccionDeApariencia(j({ cssTheme: 'Otro', enabledCssSnippets: [] }))).not.toBe(a);
  });

  it('el remoto gana en el tema y el local conserva su modo y sus fuentes', () => {
    const base = j({ cssTheme: 'Anu', theme: 'obsidian', baseFontSize: 16 });
    const local = j({ cssTheme: 'Anu', theme: 'moonstone', baseFontSize: 18 });
    const remoto = j({ cssTheme: 'Blue Topaz', theme: 'obsidian', baseFontSize: 16 });
    const r = JSON.parse(mergeDeApariencia(base, local, remoto));
    expect(r.cssTheme).toBe('Blue Topaz');   // viaja
    expect(r.theme).toBe('moonstone');       // local
    expect(r.baseFontSize).toBe(18);         // local
  });

  it('un cambio SOLO local del tema no se pisa con el remoto viejo', () => {
    const base = j({ cssTheme: 'Anu' });
    const local = j({ cssTheme: 'Cosmic' });
    const remoto = j({ cssTheme: 'Anu' });
    expect(JSON.parse(mergeDeApariencia(base, local, remoto)).cssTheme).toBe('Cosmic');
  });

  it('JSON inválido: gana el local', () => {
    expect(mergeDeApariencia('{roto', j({ cssTheme: 'Cosmic' }), j({ cssTheme: 'Anu' })))
      .toBe(j({ cssTheme: 'Cosmic' }));
  });
});
```

**Step 2: Correr y ver el fallo**

Run: `npm test -w @ovaultd/core -- apariencia`
Expected: FAIL — no existe `../src/apariencia.js`.

**Step 3: Implementación mínima**

```ts
// packages/core/src/apariencia.ts
/**
 * `appearance.json` es config COMPARTIDA solo a medias: el tema elegido y los snippets
 * habilitados son decisiones del usuario y viajan; el modo claro/oscuro (y las fuentes)
 * los reescribe Obsidian por dispositivo y NUNCA se aplican del remoto. Sincronizar el
 * archivo entero generó 25 revisiones y colisión infinita entre 3 devices.
 */
const CAMPOS_PORTABLES = ['cssTheme', 'enabledCssSnippets'] as const;

export const esApariencia = (path: string): boolean => /(^|\/)appearance\.json$/.test(path);

type Json = Record<string, unknown>;

const parsear = (texto: string): Json | null => {
  try {
    const v = JSON.parse(texto) as unknown;
    return v && typeof v === 'object' && !Array.isArray(v) ? (v as Json) : null;
  } catch {
    return null;
  }
};

/** Proyección canónica: SOLO los campos portables, con los snippets ordenados. */
export function proyeccionDeApariencia(texto: string): string {
  const o = parsear(texto) ?? {};
  const p: Json = {};
  for (const campo of CAMPOS_PORTABLES) {
    const v = o[campo];
    if (campo === 'enabledCssSnippets' && Array.isArray(v)) {
      p[campo] = [...v].map(String).sort();
    } else if (v !== undefined) {
      p[campo] = v;
    }
  }
  return JSON.stringify(p);
}

/**
 * Merge por campo (no 3-vías de texto, que con JSON por líneas es basura):
 * `{...local, ...portablesDelRemoto}`. Un campo portable solo se toma del remoto si el
 * remoto lo CAMBIÓ respecto de la base (así un cambio local del tema no se pisa con el
 * remoto viejo). El resto del archivo es local.
 */
export function mergeDeApariencia(base: string, local: string, remoto: string): string {
  const l = parsear(local);
  const b = parsear(base);
  const r = parsear(remoto);
  if (l === null || b === null || r === null) return local;

  const salida: Json = { ...l };
  for (const campo of CAMPOS_PORTABLES) {
    if (JSON.stringify(r[campo]) !== JSON.stringify(b[campo])) {
      salida[campo] = r[campo];
    }
  }
  return JSON.stringify(salida, null, 2) + '\n';
}
```

**Step 4: Correr y ver el verde**

Run: `npm test -w @ovaultd/core -- apariencia`
Expected: PASS (5 tests).

**Step 5: Commit**

```bash
git add packages/core/src/apariencia.ts packages/core/test/apariencia.test.ts
git commit -m "feat(core): merge por campo de appearance.json (tema y snippets portables)"
```

---

## Task 2: Entrar al filtro (lista blanca)

**Objective:** que `appearance.json` deje de estar excluido y se encole.

**Files:**
- Modify: `packages/core/src/SyncFilter.ts:58-61` (lista `CONFIG_PORTABLE`)
- Test: `packages/core/test/sync-filter.test.ts`

**Step 1: Test (falla)**

```ts
expect(esConfigPortable('appearance.json')).toBe(true);
expect(esConfigPortable('appearance.json.bak')).toBe(false);
```

Run: `npm test -w @ovaultd/core -- sync-filter` → FAIL.

**Step 2: Implementación**

```ts
const CONFIG_PORTABLE: readonly RegExp[] = [
  /^snippets\/.+/, // CSS propio del vault
  /^themes\/.+/, // temas instalados como archivos (contenido puro)
  /^appearance\.json$/, // tema + snippets: merge POR CAMPO (ver apariencia.ts)
];
```

Y actualizar el comentario del bloque (la viñeta que hoy explica por qué está afuera:
agregar "desde 0.2.31 viaja POR CAMPO — el modo y las fuentes siguen locales").

**Step 3: Verde + commit**

Run: `npm test -w @ovaultd/core -- sync-filter` → PASS.

```bash
git commit -am "feat(core): appearance.json en la lista blanca (merge por campo)"
```

---

## Task 3: Hash por proyección en el scan (que el modo no se suba)

**Objective:** cambiar el modo claro/oscuro no debe verse como "archivo modificado" ni
generar una revisión. El hash que compara el diff es el de la **proyección**.

**Files:**
- Modify: `packages/core/src/scan.ts` (donde se calcula `contentHash`, ~línea 100-140)
- Test: `packages/core/test/scan.test.ts`

**Step 1: Test (falla)**

```ts
// dos appearance.json que difieren SOLO en `theme` producen el mismo contentHash
const a = await escanearVault(portCon({ '.obsidian/appearance.json': j({ cssTheme: 'Anu', theme: 'obsidian' }) }), keys);
const b = await escanearVault(portCon({ '.obsidian/appearance.json': j({ cssTheme: 'Anu', theme: 'moonstone' }) }), keys);
expect(a[0]!.contentHash).toBe(b[0]!.contentHash);
```

Run: `npm test -w @ovaultd/core -- scan` → FAIL (hoy hashea los bytes).

**Step 2: Implementación**

```ts
import { esApariencia, proyeccionDeApariencia } from './apariencia.js';
// ...
const bytes = contenidos.get(rel) ?? (await port.read(rel));
const contentHash = esApariencia(rel)
  ? sha256Hex(new TextEncoder().encode(proyeccionDeApariencia(new TextDecoder().decode(bytes))))
  : sha256Hex(bytes);
```

**Step 3: Ojo con el apply**

`apply-runner` compara `hashActual` (del archivo local) contra `base.contentHash`. Si el
base viene del hash proyectado, el local también tiene que hashearse igual. Donde el
apply-runner calcula `sha256Hex(bytes)` para `appearance.json`, usar la proyección (una
función `hashDePath(path, bytes)` en `apariencia.ts` que centralice esto; el scan y el
apply llaman a la misma).

**Step 4: Verde + commit**

Run: `npm test -w @ovaultd/core` → 158+ PASS.

```bash
git commit -am "feat(core): hash proyectado de appearance.json (el modo no genera revisión)"
```

---

## Task 4: Enganchar el merge por campo en el apply

**Objective:** cuando el remoto trae un `appearance.json` que difiere del local, mergear por
campo en vez de bloquear o usar el 3-vías de texto.

**Files:**
- Modify: `packages/core/src/apply-runner.ts:228-260` (`intentarFusionar`)
- Test: `packages/core/test/merge-apply.test.ts`

**Step 1: Test (falla)**

```ts
it('appearance.json: el tema del remoto gana y el modo local se conserva', async () => {
  // base: Anu/obsidian · local: Anu/moonstone (cambió el modo)
  // remoto: Cosmic/obsidian (cambió el tema)
  const aplicado = await correrApply(/* ... */);
  const final = JSON.parse(leerLocal());
  expect(final.cssTheme).toBe('Cosmic');
  expect(final.theme).toBe('moonstone');
});
```

Run: `npm test -w @ovaultd/core -- merge-apply` → FAIL (hoy: conflicto/bloqueo o texto).

**Step 2: Implementación**

```ts
import { esApariencia, mergeDeApariencia } from './apariencia.js';
// ... dentro de intentarFusionar, después de leer textoBase/ours/theirs:
if (esApariencia(revision.path)) {
  return {
    fusion: new TextEncoder().encode(mergeDeApariencia(textoBase, textoOurs, textoTheirs)),
    oursHash,
    hashRemoto,
  };
}
const resultado = mergeDeTexto(textoBase, textoOurs, textoTheirs);
```

**Step 3: Verde + commit**

Run: `npm test -w @ovaultd/core -- merge-apply` → PASS.

```bash
git commit -am "feat(core): merge por campo de appearance.json en el apply"
```

---

## Task 5: E2E de dos réplicas (lab)

**Objective:** probar el caso real: A cambia el tema → B lo recibe; A cambia el modo → B no
cambia y no hay revisión nueva.

**Files:**
- Create: `tests/scripts/e2e-apariencia.sh` (mismo patrón que `tests/scripts/e2e-conflicto.sh`)

**Pasos del script (contra el lab `:8894`):**
1. Crear vault + dos réplicas A/B (`ovaultd setup`).
2. `A/.obsidian/appearance.json` = `{cssTheme:"Anu", theme:"obsidian"}` → sync.
3. `B` baja → `appearance.json` con `cssTheme` Anu.
4. `B` cambia **el modo** (`theme: "moonstone"`) → sync.
5. **Asserta**: no hay revisión nueva (`rev` igual) y el server no cambió.
6. `A` cambia **el tema** (`cssTheme: "Cosmic"`) → sync.
7. **Asserta**: `B` recibe `cssTheme=Cosmic` y **conserva** `theme="moonstone"`.

Run: `bash tests/scripts/e2e-apariencia.sh`
Expected: `E2E-APARIENCIA OK`.

**Commit**

```bash
git add tests/scripts/e2e-apariencia.sh
git commit -m "test(e2e): tema activo portable entre dos réplicas"
```

---

## Task 6: Release e instalación

1. Bump plugin + server a **0.2.31** (release train: el core cambia, el server no).
2. `npm test` core/CLI/plugin, `cargo test --workspace` (por el lock), `scan-leaks`.
3. `git tag 0.2.31` (plugin) + release con 3 assets.
4. Instalar en **un escritorio** y en el **teléfono** (base64; el teléfono desde la release).
5. Nota en el vault: actualizar la sección de `.obsidian` en `taxonomia-vault-reglas.md`
   (hoy dice que appearance.json no viaja) + `docs/cli.md`/CHANGELOG.

---

## Riesgos y mitigaciones

| Riesgo | Mitigación |
|---|---|
| Obsidian reordena claves o agrega campos → el hash cambia | el hash es de la **proyección**, no del archivo |
| Dos devices cambian el tema a la vez | gana el remoto; el local puede re-elegir y se propaga (1 revisión) |
| Un device con JSON viejo rompe el merge | JSON inválido → gana el local |
| El modo automático cambia al anochecer | no es campo portable; el hash proyectado lo ignora |
| `enabledCssSnippets` con orden distinto | la proyección ordena antes de hashear/mergear |

## Fuera de alcance

`community-plugins.json`, `hotkeys.json`, `core-plugins.json`, `workspace*.json` y el
`data.json` de terceros siguen locales (dependen del dispositivo).

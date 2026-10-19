---
name: ovaultd-cli
description: "Use when reading/writing notes or syncing via ovaultd CLI."
version: 1.0.0
license: MIT
platforms: [linux, macos]
metadata:
  hermes:
    tags: [Obsidian, ovaultd, CLI, CRUD, sync, notes]
---

# ovaultd CLI — cómo lo usa un agente

`ovaultd` es el cliente del vault cifrado. **El agente es un dispositivo más**: el vault
está descifrado en disco (lo mantiene `ovaultd sync`) y el agente lo edita como archivos.
El server nunca ve texto plano (E2EE) — por eso **no hay API de notas ni búsqueda
server-side**.

## Instalación (una vez por máquina)

El paquete es privado y no está en npm. Desde un checkout del repo:

```bash
npm run build -w @ovaultd/cli
bash scripts/instalar-cli.sh          # deja ~/.local/bin/ovaultd en el PATH
ovaultd --version
```

Requiere `node` en el PATH. El comando es un shim sobre el `dist` del checkout (el sync
levanta Chromium con Playwright; no hay binario autónomo).

## Reglas de oro

1. **Nunca edites el archivo del vault con `>` / `sed`**: usá `ovaultd write` (confinamiento
   al vault + rename atómico). El editor del agente no debe poder salirse del vault.
2. **El contenido va por `--stdin`**, nunca por argv: el markdown con backticks se
   ejecutaría como command substitution.
3. **Después de escribir o borrar, `ovaultd sync`** (salvo que haya un servicio/timer de
   sync corriendo). Sin sync, el cambio queda solo en esta máquina.
4. **Antes de crear una nota, buscá la canónica** (`ovaultd ls` + índice local si existe).
   Mismo sujeto → sección dentro de la existente, no nota paralela.
5. **Rutas relativas al vault**, sin `..`, sin absolutas, sin `.` al final: se rechazan.
6. La password del vault se pide **una vez** y se cachea (0600). Si no hay TTY
   (timer/CI), usá `OVAULTD_PASSWORD` o `--password-stdin`.

## Comandos

| Comando | Qué hace |
|---|---|
| `ovaultd login <server> --email <email>` | crea el device y guarda el token (0600) |
| `ovaultd list-remote` | lista vaults del server (id, nombre, rol, rev) |
| `ovaultd setup <id> --dir <carpeta>` | vincula una carpeta local al vault |
| `ovaultd sync [id]` | una pasada: sube cambios locales y baja remotos |
| `ovaultd sync --continuous` | queda corriendo (servicio) |
| `ovaultd status` | sesión, server y vaults locales |
| `ovaultd list-local [id]` | scan + hash-diff, **sin red** |
| `ovaultd cat <ruta>` | imprime un archivo (binario-safe) |
| `ovaultd write <ruta> --stdin` | escribe (crea carpetas; rename atómico) |
| `ovaultd ls [prefijo] [--json] [--todo]` | lista (`.obsidian/` afuera salvo `--todo`) |
| `ovaultd rm <ruta> [-r]` | borra |
| `ovaultd history <ruta>` | revisiones de un archivo (op, fecha, device, tamaño) |
| `ovaultd preview <ruta> --rev <n> --diff` | muestra una versión sin escribir |
| `ovaultd restore <ruta> --rev <n>` | restaura como revisión NUEVA |
| `ovaultd olvidar-clave [id]` | borra las subclaves cacheadas |
| `ovaultd unlink <id>` | desvincula la carpeta (el server no se toca) |

Con más de un vault vinculado, casi todos aceptan `--vault <n>`.

## Flujo: agregar una nota

```bash
# 1. ¿ya existe? (nombre y, si el host tiene indexador, contenido)
ovaultd ls | grep -i "<tema>"

# 2. escribir (el contenido va por stdin)
printf '%s' "$CONTENIDO" | ovaultd write "02-hermes-docs/<tema>/<nota>.md" --stdin

# 3. publicar
ovaultd sync

# 4. verificar que quedó
ovaultd cat "02-hermes-docs/<tema>/<nota>.md" | head -20
```

Si hay un servicio de sync (timer o `--continuous`), el paso 3 es opcional y el cambio se
publica solo.

## Flujo: editar sin pisar

```bash
ovaultd cat "ruta/nota.md" > ~/tmp/nota.md      # leer (o a un archivo temporal)
# ... editar ~/tmp/nota.md ...
ovaultd write "ruta/nota.md" --file ~/tmp/nota.md
```

Si otro dispositivo editó el mismo archivo, el sync resuelve: fusión 3-vías si las
ediciones no se solapan, o **copia de conflicto** (`<nombre> (conflicto <device> <fecha>).md`)
conservando ambos. Nada se pierde y el ciclo no se bloquea.

## Flujo: buscar

La búsqueda es **local** (el server no indexa). En orden:

1. **Índice local** (si el host lo tiene): `obsn rag "consulta" 5` (semántica) y
   `obsn search "<nombre>"` (léxica). Reindexá tras cambios masivos.
2. **Sin indexador**: `grep -ril "término" <carpeta-del-vault>` — es un vault descifrado en
   disco; grep es suficiente y no depende de nadie.
3. **Nunca** busques contra el server: no hay endpoint y no puede haberlo (E2EE).

## Historial y recuperación

```bash
ovaultd history "ruta/nota.md"                 # revisión, fecha, device, tamaño
ovaultd preview "ruta/nota.md" --rev 12 --diff # comparar con el local
ovaultd restore "ruta/nota.md" --rev 12        # restaura (sube revisión NUEVA)
```

La historia permite recuperar cualquier versión retenida: **un borrado no es definitivo**
si la revisión sigue en el oplog.

## Errores comunes → causa

| Mensaje | Causa / qué hacer |
|---|---|
| `no hay TTY para el prompt` | comando en timer/CI sin `--password-stdin` ni `OVAULTD_PASSWORD` |
| `no hay sesión` | falta `ovaultd login <server> --email ...` |
| `hay N vaults vinculados; indicá cuál` | pasá `--vault <n>` (o el id en `sync <id>`) |
| `ruta inválida: travesía / absoluta` | la ruta debe ser relativa y sin `..` |
| `el vault #N no está vinculado a una carpeta` | `ovaultd setup <N> --dir <carpeta>` |
| `HTTP 401` | password de cuenta incorrecta (no es la del vault) |
| `el server no respondió en 30 s` | red/borde; el request tiene timeout, no se cuelga |
| `no se pudo conectar` | server caído o URL mal; probá `ovaultd status` |

## Seguridad

- El estado local (`~/.config/ovaultd/`, 0600) incluye el token y las **subclaves
  cacheadas**: es tan sensible como el vault. No lo imprimas, no lo copies a repos.
- `ovaultd olvidar-clave` borra las subclaves (el próximo comando pide la password).
- La password de la **cuenta** (login) y la del **vault** (setup) son distintas. Ninguna
  se persiste.
- El server es ciego: si ves un endpoint que promete leer/buscar notas, no es de este
  sistema.

## Convenciones del vault (ajustables)

Ejemplo de taxonomía por **quién escribe** (no por tema):

| Autor | Carpeta |
|---|---|
| Agente / documentación | `02-hermes-docs/<tema>/` + tag `#hermes-docs` + entrada en `00-INDEX.md` |
| Dueño del vault | `01-Technical/`, `04-Ideas/`, `03-Personal/` |
| Credenciales | `03-Personal/Secrets/` (nunca citar ni linkear) |
| Archivo | `05-Archive/`, `06-Todo/` |

Crear carpeta nueva solo si hay **≥3 notas** del mismo tema y no entran en una existente.

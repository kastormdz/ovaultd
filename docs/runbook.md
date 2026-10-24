# Runbook de operación

Este documento es operativo: los comandos se corren tal cual y están verificados contra el
repo. El gate automatizado de toda la cadena es `cargo xtask backup-restore` (smoke fresco →
backup → restore verificado → rechazo sin `--force` → diff byte a byte).

## Backup

### Estándar (con el stack de Docker Compose detenido)

```bash
bash scripts/backup.sh \
  --db  <dir-de-datos>/metadata.db \
  --cas <dir-de-datos>/cas \
  --output /ruta/backups/ovaultd-$(date +%F).tar.zst \
  --stop-cmd  "docker compose -f compose.yaml stop ovaultd" \
  --start-cmd "docker compose -f compose.yaml start ovaultd"
```

- El server **baja para el backup** (quietud total: la alternativa simple del plan) y
  **vuelve a subir siempre**, también si el backup falla a mitad.
- Produce `<output>` + `<output>.sha256`. Adentro: `metadata.db` (copia consistente por la
  API de backup de SQLite, WAL incluido), todo el `cas/` y un `manifest.json` con los
  sha256 de cada archivo y los conteos de la base. Los `.tmp-*` huérfanos del CAS quedan
  afuera (basura no referenciada).
- El original **nunca** se toca: solo lectura + staging temporal.
- Requiere `python3` y `zstd` (están en el host de build; el contenedor no los necesita).

### Cron sugerido

```cron
17 4 * * * cd /ruta/al/repo && bash scripts/backup.sh --db /srv/ovaultd/datos/metadata.db \
  --cas /srv/ovaultd/datos/cas --output /srv/backups/ovaultd-$(date +\%F).tar.zst \
  --stop-cmd "docker compose -f compose.yaml stop ovaultd" \
  --start-cmd "docker compose -f compose.yaml start ovaultd" \
  >> ~/logs/ovaultd-backup.log 2>&1
```

Retención: el script no borra nada; rotar por nombre de archivo (p. ej. 7 diarios +
4 semanales). Un backup no verificado no es un backup: correr el restore de prueba
(abajo) al menos una vez por semana.

## Restore

### 1. Verificación (siempre a un destino NUEVO)

```bash
bash scripts/restore.sh --input /srv/backups/ovaultd-2026-10-17.tar.zst \
  --into /srv/ovaultd-restore-check --project ovaultd-restore-check
```

Verifica, en orden: sha256 del archivo (sidecar), **cada** entrada del manifiesto
(base + todos los blobs), `PRAGMA integrity_check`, `PRAGMA foreign_key_check` y **todas**
las referencias índice→CAS. Un destino no vacío se **rechaza** salvo `--force`. Al final
imprime el resumen: árbol, historial, firmas y wrappers.

### 2. Stack de verificación

Apuntar el `OVAULTD_DATA` (bind o volumen) del compose a `/srv/ovaultd-restore-check` y:

```bash
docker compose -p ovaultd-restore-check up -d
curl -fsS http://127.0.0.1:8080/healthz
```

Probar un login real contra ese stack (el estado del servidor es completo: cuentas,
tokens y material cifrado viven en la base restaurada).

### 3. Swap (recién cuando el paso 2 verifica)

1. Bajar **ambos** stacks (`docker compose -p <project> down` y el real).
2. Renombrar volúmenes/binds: el actual pasa a `*.-viejo`; el verificado pasa a ser el actual.
3. Levantar el stack real; `healthz`; prueba de escritura y lectura real.
4. Conservar el volumen viejo **hasta el próximo backup bueno**; no borrarlo el mismo día
   del swap.

## Recuperación ante incidentes

### La base está corrupta o no abre

```bash
sqlite3 <dir-de-datos>/metadata.db "PRAGMA integrity_check;"
```

Si no devuelve `ok`: restore del último backup verificado (procedimiento de arriba). No
existe reparación in-place confiable para una SQLite corrupta; el backup ES la recuperación.

### Faltan archivos del CAS

El mismo chequeo del restore, suelto:

```bash
python3 - <dir-de-datos> <<'PY'
import os, sqlite3, sys
base = sys.argv[1]
con = sqlite3.connect(os.path.join(base, "metadata.db"))
faltan = []
for vault_id, h in con.execute("SELECT vault_id, hash FROM blobs"):
    hexh = h.hex()
    if not os.path.isfile(os.path.join(base, "cas", hexh[:2], hexh)):
        faltan.append((vault_id, hexh))
print(f"faltantes: {len(faltan)}", faltan[:5])
PY
```

Los blobs no se reconstruyen (son ciphertext del cliente): la única recuperación es un
backup. Los **huérfanos** (archivo sin fila) son lo inverso y no rompen nada: basura
recuperable que el GC de la fase 5 va a barrer.

### El servidor no arranca

```bash
docker compose logs --tail 50 ovaultd     # logs JSON; nunca hay tokens ni payloads
docker inspect ovaultd-ovaultd-1 --format '{{.State.Health.Status}}'
```

- `unable to open database file`: permisos del volumen (uid 10001 del contenedor); ver
  la sección de troubleshooting en `docs/deploy.md`.
- Puerto ocupado: `OVAULTD_BIND`.
- Base degradada: `/healthz` devuelve 503 y el log lo dice.

### Pérdida total del host

1. Host nuevo + Docker + `git clone` del repo.
2. `restore.sh` sobre un directorio nuevo (pasos de arriba, destino NUEVO directo).
3. Levantar el stack real apuntando ahí; cada cliente entra con su password o su recovery
   key (las cuentas ya están en la base: no se recrean).
4. Un cliente que tenía cambios locales sin subir los reenvía solo: su outbox local es
   recuperable y el bootstrap del servidor restaurado lo acepta (si el `since` local quedó
   por delante, el server responde `min_rev` y el cliente hace bootstrap coherente **sin
   borrar su outbox**).

## Verificaciones periódicas

- Semanal: `cargo xtask backup-restore` en el host de desarrollo (cadena completa en
  ~1 minuto) y/o un restore del backup real más reciente a un tmpdir.
- Mensual: revisar `.tmp-*` huérfanos en el CAS (`find <cas> -name '.tmp-*' | wc -l`), publicaciones interrumpidas; no se referencian y no entran al backup.
- Después de cada incidente: un backup manual + restore de verificación antes de
  seguir operando.

## GC de blobs huérfanos

Un blob queda huérfano cuando se publicó (upload o `blob_head`) y ningún batch lo
referenció, o cuando su única referencia era un snapshot que se borró. El GC los limpia
desde `ovaultd-admin` (el `--db` va ANTES del subcomando):

```bash
docker exec ovaultd-ovaultd-1 ovaultd-admin --db /data/metadata.db gc            # dry-run (default)
docker exec ovaultd-ovaultd-1 ovaultd-admin --db /data/metadata.db gc --verify   # audita el invariante
# El borrado, con el server PARADO (el conteo no debe moverse mientras corre):
docker compose stop ovaultd
docker compose run --rm --entrypoint ovaultd-admin ovaultd --db /data/metadata.db gc --delete
docker compose up -d
```

- `gc` (sin flags) es **dry-run**: lista los candidatos con tamaño y fecha, no escribe
  NADA. La gracia por defecto es de 24 h (`--gracia-minutos N`): un blob recién publicado
  puede tener su batch en vuelo (su `refs` es 0 hasta el commit).
- `gc --verify` **audita** el invariante `refs = revisiones con blob + snapshots`,
  recontando las fuentes y comparándolas en ambos sentidos. Solo lee.
- `gc --delete` borra de verdad (filas + archivos del CAS), en una transacción, y
  **excluye los uploads en curso**.
- `--vault N` limita cualquier modo a un vault.

Si `--verify` reporta discrepancias, es un bug de un camino que ajusta referencias: NO
borres hasta entender la diferencia (`refs` vs `esperado` dicen para dónde mirar).

## Failpoints (simulación de crash, solo builds debug)

El server inyecta fallas con `OVAULTD_FAILPOINT=<punto>` y el modo con
`OVAULTD_FAILPOINT_MODE`:

- `abort` (default): `std::process::abort()`: crash real (SIGABRT), como un corte del
  proceso. Es el modo del test de proceso real.
- `panic`: desenrolla, para los tests in-process que capturan el unwind.

Puntos: `cas.temp`, `cas.fsync`, `cas.publish` (camino de publicación de blobs),
`sql.commit` (commit del batch), `ack.put_blob` (entre el registro y el ACK HTTP).

En un binario **release** (el del contenedor) son no-op: no existe forma de activarlos en
producción. La suite que los ejercita:

```bash
cargo test -p ovaultd-server --test crash_commit
```

Cubre: crash después del temporal / fsync / publicación (el blob queda huérfano, nunca como
archivo visible), ACK perdido que no duplica revisión, y un **SIGABRT real del binario a
mitad de un PUT** con verificación desde afuera (base íntegra + subida completada por un
proceso nuevo).

## Panel sin admins (recuperación)

El servidor nunca deja el panel sin admins: degradar o deshabilitar al último rebota con 409
(el conteo va dentro del `UPDATE`, no en un `SELECT` previo). Si por un restore viejo o una
edición manual el panel no tiene a nadie con rol `admin`:

```bash
# 1) Mirar quién hay (nada de esto descifra contenido)
docker exec ovaultd-ovaultd-1 ovaultd-admin --db /data/metadata.db inspect

# 2) Crear un admin nuevo (password por stdin, nunca por argumento)
printf '%s\n' '<password>' | docker compose exec -T ovaultd \
  ovaultd-admin create-account --email vos@ejemplo.com --role admin
```

Si la cuenta ya existe y sólo perdió el rol, se promueve en el panel desde otra cuenta admin
o, como último recurso, con SQL directo sobre `accounts.role`.

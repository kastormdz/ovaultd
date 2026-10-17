# migraciones

**Nada que no sea una migración vive en este directorio.** `sqlx::migrate!("../../migrations")` lee **todos** los
`.sql` de acá y los embebe en el binario al compilar: un backup, un borrador o un archivo suelto se convierte en una
migración más (o en una versión duplicada) y el server falla al arrancar con un error que habla de un archivo que vos
nunca pensaste como migración.

- Backups y borradores: **fuera de acá** (por ejemplo `docs/backups/`).
- El contenido se embebe en tiempo de compilación: **cambiar un `.sql` sin recompilar no cambia nada** (el binario
  sigue llevando el viejo). Si un cambio en una migración "no hace efecto", forzá el rebuild.
- Los `PRAGMA` no van en las migraciones: corren dentro de una transacción y SQLite rechaza `synchronous` ahí adentro.
  La configuración de la conexión vive en `db::connect`.

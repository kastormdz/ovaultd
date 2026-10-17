# ADR-0010 — Consultas SQLx: API de ejecución con `FromRow`, **no** las macros de verificación en compilación

- **Fecha:** 2026-09-25
- **Estado:** **aceptado**
- **Origen:** `auth.rs` (el primer archivo con consultas) usó `sqlx::query!` y el crate dejó de compilar sin una base viva

## El hecho, medido

```
error: set `DATABASE_URL` to use query macros online, or run `cargo sqlx prepare` to update the query cache
  --> crates/ovaultd-server/src/auth.rs:53:21
```

`sqlx::query!` / `query_as!` verifican el SQL **contra una base real en tiempo de compilación**. Eso tiene un precio
concreto en este proyecto:

1. **`cargo check` deja de funcionar sin base.** Y la aceptación de la fase 1 son comandos:
   `cargo test -p ovaultd-server --test auth|cas|delta|blobs`, más `server-smoke.sh`. Un repo donde no se puede compilar
   sin levantar un servicio antes es un repo donde el CI necesita un servicio y donde el que llega nuevo no arranca.
2. **La verificación depende del estado de ESA base.** Si el esquema cambió y el cache no se regeneró, compila contra
   un esquema viejo: el error aparece en runtime, que es justo lo que se quería evitar.
3. La alternativa del cache offline (`cargo sqlx prepare` + `.sqlx/`) agrega un artefacto generado que hay que
   mantener sincronizado con cada migración, a mano y en cada PR.

## Decisión

Se usa la **API de ejecución** de SQLx, con el tipado puesto por **`#[derive(sqlx::FromRow)]`** en los structs:

```rust
#[derive(sqlx::FromRow)]
struct FilaCuenta { id: i64, email: String }

let cuenta = sqlx::query_as::<_, FilaCuenta>("SELECT id, email FROM accounts WHERE id = ?")
    .bind(id)
    .fetch_optional(pool)
    .await?;
```

- Bind **siempre** por parámetro (`.bind(...)`): nunca SQL armado con `format!`/`format_args!`. Inyección no es una
  opción en un servidor que recibe `path_id` y `blob_id` por la red.
- El tipado sigue existiendo: `FromRow` mapea columnas a campos con tipo, y el motor valida en el `prepare` de SQLx en
  runtime (que es donde el error tiene que aparecer: contra el esquema **vivo**, que es el único que importa).
- `fetch_optional` / `fetch_one` / `rows_affected()` explícitos: nada de asumir que hay fila.

## Consecuencia sobre el plan

`docs/build-plan.md` dice "Usar SQLx con consultas tipadas y migraciones". **Se mantiene**: el tipado viene de
`FromRow`, no de la macro. Este ADR precisa cómo se lee esa línea, porque "consultas tipadas" se puede leer como
"`query!`" y esa lectura rompe el arranque del proyecto.

## Lo que NO cambia

- Siguen las migraciones con `sqlx::migrate!` (eso sí es una macro, pero sólo lee `migrations/` en el manifest: no
  necesita base ni `DATABASE_URL` para compilar).
- Sigue todo por parámetros bindeados y sigue sin haber PostgreSQL como dependencia.
- El esquema no se toca.

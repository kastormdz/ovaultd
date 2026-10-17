# ADR 0012 — Subida reanudable (tus core) y reservas de cuota

- **Fecha:** 2026-10-16
- **Estado:** aceptado
- **Origen:** paso 5 de la fase 1 (`docs/build-plan.md`), ADR-0004

## Contexto

ADR-0004 fijó que los blobs grandes van **siempre** por HTTP reanudable, pero dejó el wire sin fijar
("`Content-Range` / `tus`"). El wire no puede quedar implícito: cliente y servidor tienen que coincidir
exactamente en **dónde se reanuda** una subida interrumpida, y el servidor no puede creerle a la
longitud declarada. Además la cuota opt-in necesita una regla de concurrencia: dos subidas simultáneas
no pueden pasar las dos por el mismo margen.

## Decisión

1. El camino HTTP reanudable es un **subset de tus 1.0** (creación + core; sin las extensiones de
   checksum, termination ni concatenation). Se descarta `Content-Range` como mecanismo de reanudación
   (sigue siendo el de `GET`).
2. Wire: `POST /vaults/{id}/blobs` crea (con `Upload-Length` y `Upload-Metadata: blob_id <b64>`),
   `HEAD /vaults/{id}/uploads/{upload_id}` dice dónde reanudar, `PATCH` sube bloques con
   `Upload-Offset`. `Tus-Resumable: 1.0.0` obligatorio en las tres.
3. La longitud declarada **no se cree**: un bloque que pasa el total termina la subida (nunca un
   truncado silencioso); al completar se verifica el hash **real** del archivo contra `blob_id` antes
   de publicar. Completar = CAS → índice → `offset = total`, en ese orden: cualquier fallo deja la
   subida reanudable y el chunk final reintentado es idempotente.
4. Dedupe: si el blob ya está registrado para el vault, la subida **nace completa**
   (`offset == total`: no reserva cuota ni escribe bytes).
5. Cuotas **opt-in** (`quota_bytes NULL` = ilimitado, es política, no seguridad): la creación reserva
   el total en **una sola sentencia** (`INSERT … SELECT` con guard), la reserva viva de una subida
   es su `total` completo (los bytes ya recibidos viven en el `.part`; los que faltan están
   comprometidos) y la suma `blobs.size + reservas` no puede superar la cuota. `PUT` y el
   WS inline consultan el mismo límite **sin** reserva: la ventana de carrera entre dos subidas
   chicas simultáneas es chica y se acepta (la alternativa era serializar todas las subidas).
6. Vencimiento: TTL por subida (default 24 h, `OVAULTD_UPLOAD_TTL_HOURS`); el barrido perezoso (corre
   al crear una subida) borra fila + temporal y libera la reserva.

## Consecuencias

- La reanudación es un estándar público (tus), no una invención: el cliente TS de las fases 2–3
  implementa contra la spec y los tests de integración la ejercitan.
- El estado de una subida vive en SQLite (offset confirmado) + un `.part` en `cas_root/.uploads/`:
  reiniciar el server no pierde el punto de reanudación.
- Los errores siguen la lista cerrada: offset que no sigue → **409** con `Upload-Offset` actual
  (cuerpo `bad_request`); hash que no coincide / exceso sobre lo declarado → `bad_request` (400) +
  limpieza total de la subida.
- Queda fuera (fase posterior): la extensión de `termination` (DELETE de uploads ajenos), la de
  `checksum` y las subidas multi-instancia (SQLite sigue siendo single-writer).

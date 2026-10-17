# ADR 0004 — Un solo transporte por blob

- **Fecha:** 2026-09-25
- **Estado:** aceptado
- **Origen:** § 17.5 de la spec canónica

## Contexto

La spec dejaba **dos caminos** para subir el mismo blob (frames binarios por WebSocket, y HTTP reanudable
`POST /vaults/{id}/blobs`) sin decir cuál gana. Dos caminos para la misma operación significan, en la práctica:
duplicación de lógica de reanudación, dos lugares donde perder un `offset`, y bugs de "subí por el otro camino y el
`transfer` quedó colgado". El protocolo v1 además **no reanudaba**: reenviaba el archivo completo.

## Decisión

Un **umbral configurable** decide el camino, y es **excluyente**:

| Tamaño | Camino |
|---|---|
| ≤ umbral (default 1 MB) | **inline** en frames binarios del WS (`blob_head` → N frames, `transfer_id` 8 B + `offset` 8 B) |
| > umbral | **siempre** HTTP reanudable con `Content-Range` / `tus` |

- **Nunca los dos para el mismo blob**, en ninguna dirección.
- El prefijo binario lleva **`offset` explícito** ⇒ reanudación real (el servicio oficial reenvía el archivo entero).
- El receptor mantiene una **cola** de frames, no un único deferred: un frame que llega antes de que el handler esté
  listo se bufferea en vez de descartarse (bug real del cliente oficial, corregido en su v0.0.3).
- Subida al CAS en **dos fases**: temporal → `fsync` → mover al CAS → recién ahí referenciar en la transacción SQL.

## Consecuencias

- El umbral es la única perilla; cambiarlo no cambia el protocolo ni requiere negociación.
- El camino HTTP necesita las **mismas** garantías de autorización que el WS (token + ACL del vault en la ruta).
- Los blobs viven scoped al vault: `GET /vaults/{id}/blobs/{hash}`. Un `blob_id` suelto no es una URL válida.

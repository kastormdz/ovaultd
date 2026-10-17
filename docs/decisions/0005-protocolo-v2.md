# ADR 0005 — Protocolo v2: handshake firmado, ACL por vault y alcance operativo

- **Fecha:** 2026-09-25
- **Estado:** aceptado
- **Origen:** § 17.5 de la spec canónica (hallazgos de seguridad y de producto)

## Contexto

Cinco cosas del v1 no eran decisiones tomadas a conciencia, sino restos de haber copiado el modelo del servicio pago
o de haber dejado un endpoint sin pensar:

1. El acceso se probaba con un `keyhash_proof` cuyo mecanismo **nunca se definió** (¿HMAC de qué, contra qué?).
2. `GET /blobs/{hash}` **no tenía scope de vault**: un blob no estaba atado a su vault en la ruta ni pasaba por ACL.
3. El vault arrastraba **`region`** y las cuentas **tiers comerciales de cuota (5 / 200 MB)** — decisiones de SaaS.
4. **No había forma de retirar un colaborador ni de abandonar** un vault compartido (desvincular un *device* no es lo mismo).
5. **No se podía previsualizar una versión** antes de restaurarla.

## Decisión

1. **Handshake firmado.** `hello` → `challenge` (32 B efímeros) → `t:auth` con
   `Ed25519_sign(k_sign, challenge § vault_id § device_id)`. El server valida con la **clave pública** del vault
   (§ ADR-0002): nada almacenado permite forjar un proof, y el challenge mata el replay.
2. **Blobs scoped y autorizados:** `GET /api/v1/vaults/{id}/blobs/{hash}`, con ACL del vault. Se elimina la ruta global.
3. **Se elimina `region`** del vault (single-region por diseño) y **se eliminan los tiers de cuota**: self-hosted sin
   límites por defecto, `quota_bytes` / `max_file_bytes` **opt-in** (`NULL` = ilimitado), enforcement en la subida.

> **Enmendado 2026-09-25 (parcial) — ver ADR-0007.** El bullet de arriba mezclaba dos cosas distintas. `quota_bytes` **sigue opt-in** (es política: cuánto querés dar). `max_file_bytes` **ya no**: pasa a tener **techo duro de protocolo de 32 MiB** (`MAX_FILE_BYTES`), porque el contenido se cifra con un AEAD de mensaje único y el costo de memoria es **O(~4n)** (`docs/crypto-gate.md` H-3). Por vault se puede bajar, nunca superar. El resto del ADR sigue vigente sin cambios.
4. **Membresía completa:** `POST/DELETE /vaults/{id}/members[/{account}]` y **`POST /vaults/{id}/leave`**. Retirar a
   alguien revoca su acceso al vault (los blobs ya descargados, obviamente, no se pueden recuperar).
5. **`GET /vaults/{id}/history` devuelve `path_enc` y `prev_path_id`**, y la UI/CLI permiten **previsualizar** una
   revisión antes de restaurar (RF-52).
6. **Terminal:** el host debe ofrecer **TLS ≥ 1.2 (1.3 preferido)**, sin suites débiles, y el cliente **valida el host
   devuelto** por el server. Instalación LAN con **CA propia documentada**. `proto` negocia la app, **no** es seguridad.
7. **Cuotas y limites son opt-in** *(matizado por ADR-0007: `quota_bytes` sigue opt-in, pero el tope de tamano de contenido es DURO — 32 MiB)*, y ninguna decisión de sync depende del "plan".

## Consecuencias

- Al no haber `region`, la estrategia multi-nodo (nivel C) se resuelve por **despliegue** (réplicas del mismo servicio),
  no por selector en el vault.
- Retirar a un miembro no rota la `vault_key`: si el modelo de amenaza lo exige, hay que **rotar la clave del vault**
  (ADR-0002) y eso sí re-cifra. Queda documentado como limitación, no como feature.
- La CA propia obliga a documentar el pinning/instalación: sin eso, la mitad de las instalaciones "self-hosted LAN"
  terminan con TLS desactivado y la cripto E2E tapando el agujero, que no es lo mismo.

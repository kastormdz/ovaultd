# ADR-0007 — Contenido soportado: notas, y tope duro de tamaño

- **Fecha:** 2026-09-25
- **Estado:** **aceptado**
- **Cierra:** G-6 / H-3 (`docs/crypto-gate.md`) — el último hallazgo abierto de la fase 0.2

## Contexto

`sync2` cifra el contenido con un **AEAD de mensaje único** (XChaCha20-Poly1305): `nonce_body` +
`ciphertext` en un solo mensaje, para que `blob_id = SHA-256(ciphertext)` sea la clave del CAS y el dedupe
funcione. XChaCha20-Poly1305 **no expone API incremental**.

Consecuencia medida (`js/streaming.mjs` + `js/probe-o1.mjs`, `docs/crypto-gate.md` H-3):

| | Medición |
|---|---|
| Pasada 1 (nonce, HMAC incremental) | **O(1)** — delta de 8 a 14 MiB con archivos de 8 / 64 / 256 MiB (no escala) |
| Pasada 2 (AEAD de mensaje único) | **O(~~4n)** — 347 MiB de RSS y 257 MiB de `arrayBuffers` para un archivo de 64 MiB |

Las dos salidas posibles eran: **(a)** aceptar el costo O(n) con un tope explícito, o **(b)** pasar a AEAD **por chunk
con manifiesto**, que da memoria acotada real pero **cambia `sync2`** (otro formato de blob, otro `blob_id`, re-verificar
los 32 vectores y romper el dedupe por contenido completo).

## Decisión

**(a) Se acepta el costo O(n), con un tope duro de tamaño.** El alcance del MVP son **notas de texto** (markdown +
frontmatter), no multimedia. El caso de uso no llega ni cerca del límite, y (b) costaría reescribir el formato de
contenido de `sync2` para protegerse de un caso que no existe.

### Alcance declarado

| Soportado | **No** soportado (rechazado explícitamente, nunca en silencio) |
|---|---|
| Markdown y texto (`.md`, `.txt`, `.canvas`, `.base`) | Video |
| Archivos chicos (imágenes, PDFs, adjuntos de nota) hasta el tope | Binarios grandes / discos de imágenes |
| Configuración del vault (`.obsidian/*.json`) | Bases de datos o blobs generados por otros plugins |

### El tope

| Parámetro | Valor | Dónde se aplica |
|---|---|---|
| `MAX_FILE_BYTES` | **32 MiB** (`33554432`) | Constante del protocolo, **igual en cliente y servidor** |
| `vaults.max_file_bytes` | `NULL` = usar el techo del protocolo; por vault se puede **bajar**, **nunca** superar | Autoridad final |
| `vaults.quota_bytes` | cuota total por vault (sigue siendo opt-in: es política) | Autoridad final |

**Corrección al spec en el mismo acto:** `docs/protocol.md` decía `max_file_bytes` "`NULL` = ilimitado (opt-in)".
Eso queda **derogado**: con contenido O(n) en memoria, "ilimitado" es un OOM esperando su turno. `quota_bytes` sigue
opt-in porque es **política** (cuánto *querés* dar); `max_file_bytes` pasa a tener **techo duro** porque es
**seguridad** (cuánto *puede* el proceso sin morir). Son dos cosas distintas y el spec las tenía mezcladas.

### Dónde se hace cumplir (y qué pasa cuando se pasa)

1. **Cliente, antes de leer el archivo.** La regla de oro: **nunca** se materializa en memoria un archivo que no se va a
   poder subir. Si `size > tope` ⇒ no se lee, no se cifra, no se sube. El archivo queda marcado como
   **"no sincronizable"** en el índice del cliente, con motivo visible en la UI.
2. **Cliente, otra vez, en la pasada 2.** Si el archivo creció entre las dos pasadas y ahora supera el tope ⇒ `ABORT`.
3. **Servidor, al recibir el blob.** `Content-Length` y tamaño real contra `max_file_bytes` ⇒
   **``max_file_exceeded``** + el límite vigente en el mensaje de error. Y **antes** de asignar la revisión, para no dejar una
   revisión apuntando a un blob que no se va a aceptar.
4. **Servidor, en el disco.** Nunca escribir un blob parcial: se escribe a temporario y se `rename` al confirmar el
   tamaño (mismo camino que la escritura atómica del CAS).

**Rechazar es obligatorio; saltear en silencio está prohibido.** Un archivo que se saltea sin avisar es
indistinguible de una pérdida de datos, y rompe la propiedad de que el cliente pueda confiar en que "sincronizado"
significa sincronizado.

### Presupuesto de memoria resultante

Con tope de 32 MiB y **un blob en vuelo por dispositivo** (regla ya vigente del protocolo: un transporte por blob, del
`push` serializado) el pico por subida es **~4 × 32 MiB ≈ 130 MiB**, acotado y predecible. Si en el futuro se habilita
concurrencia de blobs, el límite de memoria se calcula como `tope × factor × concurrencia` y **hay que re-leer este ADR**.

### Qué rompería esta decisión

Señales de que hay que pasar a (b) AEAD por chunk (y entonces este ADR se **supersede**, no se edita):

- Aparece la necesidad de sincronizar **video o adjuntos > 32 MiB** de forma habitual.
- El tamaño **típico** de archivo del parque real se acerca al tope (no el máximo: el típico).
- El pico de memoria se convierte en un problema operativo medido (OOM del server, no una sospecha).

## Consecuencias

- ✅ `sync2` **no cambia**: los 32 vectores dorados y el gate siguen valiendo tal como están.
- ✅ El dedupe por contenido completo y `blob_id = SHA-256(ciphertext)` se mantienen intactos.
- ✅ El costo O(n) queda **cuantificado y acotado**, no escondido.
- ⚠️ Un usuario que meta un video en su vault **va a ver un rechazo**, y tiene que poder entender por qué (mensaje
  explícito en la UI, no un error genérico).
- ⚠️ El tope es **del formato de contenido**, no una preferencia: subirlo obliga a re-verificar el presupuesto de
  memoria del servidor.

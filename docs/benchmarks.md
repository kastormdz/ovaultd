# Benchmarks — LAN (fase 5, paso 6)

Números **medidos**, no estimados: cada corrida sale de `tests/scripts/bench-lan.sh` contra
un server real (no hay mocks ni simulación de red). El script se puede re-correr en
cualquier momento:

```bash
BENCH_EMAIL=… BENCH_PASSWORD=… BENCH_VAULT_PASSWORD=… \
    bash tests/scripts/bench-lan.sh          # BENCH_NOTAS=1000, BENCH_KB=100 por defecto
```

- Genera `N` notas de ~`KB` KB, crea un vault nuevo en el server, lo vincula y mide con el
  CLI del núcleo: **T1** primera subida (vault nuevo), **T2** ciclo en reposo, **T3** un
  cambio suelto y **T4** la bajada completa en un **segundo cliente aislado** (config y
  runtime propios: sin eso el segundo cliente hereda el checkpoint del primero y en vez de
  bajar sube borrados — el script lo caza con un assert).
- Credenciales SIEMPRE por entorno; el vault de prueba se borra del server al final
  (`BENCH_LIMPIAR=0` para conservarlo).

## Resultado de referencia (2026-10-18)

**1000 notas × ~100 KB = 100,0 MB** · server `0.2.7` · cliente y server en el **mismo host**
(loopback; una LAN real y el Wi-Fi del teléfono van más lento — este número es el techo del
software, no de la red) · SSD, binario `debug` del lab de desarrollo (un release real es
más rápido).

| Etapa | Qué mide | Resultado |
|---|---|---|
| **T1** primera subida | 1000 altas (vault nuevo) | **99,4 s** (~1,0 MB/s · ~99 ms por revisión) |
| **T2** ciclo en reposo | escanear los 100 MB y confirmar «sin cambios» | **8,6 s** |
| **T3** un cambio suelto | editar 1 de 1000 notas y subirla | **8,7 s** |
| **T4** bajada completa | segundo cliente baja 1001 revisiones (↓1001 · ↑0) | **39,1 s** (~2,6 MB/s · ~39 ms por revisión) |

### Corrida 2026-10-19 · núcleo 0.2.20 (push agrupado + PUTs paralelos + scan incremental)

| Fase | Qué mide | Resultado |
|---|---|---|
| **T1** primera subida | 1000 altas (vault nuevo) | **71,9 s** (~1,4 MB/s) |
| **T2** ciclo en reposo | escanear los 100 MB y confirmar «sin cambios» | **3,4 s** |
| **T3** un cambio suelto | editar 1 de 1000 notas y subirla | **3,4 s** |
| **T4** bajada completa | segundo cliente baja 1001 revisiones (↓1001 · ↑0) | **39,1 s** (~2,6 MB/s) |

Qué cambió: las operaciones se mandan **agrupadas (50 por batch, UNA revisión por lote —
el modelo del restore masivo)** y los PUT de blobs corren **en paralelo**; el escaneo reusa
el hash del índice cuando mtime+size no cambiaron (no lee los 100 MB) y pide los stats en
**un solo `statMany`** (antes: un POST por archivo al puente). El delta de un cambio y el
reposo bajaron ~80 %; la subida inicial, 29 %.

### Corrida 2026-10-19 (bis) · `statMany` en el puente de archivos

| Fase | Resultado |
|---|---|
| **T2** ciclo en reposo | **1,77 s** |
| **T3** un cambio suelto | **1,88 s** |
| **T1** primera subida | **70,7 s** |
| **T4** bajada | **38,7 s** |

Los ~1,8 s de T2/T3 **incluyen el arranque de Chromium del CLI** (~1,5 s): **el ciclo neto
quedó en ~0,3 s**, por debajo del target de un segundo — y en el plugin (motor persistente,
sin arranque) el ciclo también es ~0,3 s.

### Corrida 2026-10-19 (ter) · `readMany`, puente binario y el piso criptográfico

| Fase | Resultado | vs. anterior |
|---|---|---|
| **T1** primera subida (1000) | **70,8 s** | −1,1 s |
| **T2** ciclo en reposo | **1,93 s** | −1,5 s |
| **T3** un cambio suelto | **1,93 s** | −1,5 s |
| **T4** bajada (2º cliente) | **36,5 s** | −2,6 s |

Qué cambió: `readMany` (el escaneo lee en tandas de 100 por el puente, no un round-trip
por archivo) y el **puente binario**: los PUT de blobs van de la página al puente y de ahí
a node, sin preflight CORS (el cache del navegador es por URL y cada blob tiene la suya:
1000 OPTIONS de ~40 ms) y sin base64 de 180 KB por blob.

**Descartado con medición** (cada uno era un sospechoso razonable del T1): fsync del CAS
por blob (diferido al commit con un `syncfs` por lote: 71,9 → 70,7 s), fsync del WAL de
SQLite (A/B `synchronous=NORMAL`: 70,1 vs 70,8 s — ruido), Nagle/delayed ACK
(`TCP_NODELAY` en el listener: sin cambio, pero queda), base64 del puente (binario crudo:
sin cambio medible) y preflight CORS (sin cambio en el agregado).

**El piso real del T1**: el pipeline criptográfico del cliente. `CryptoProvider` usa
`@noble/hashes` y cifradores JS puros: hashear y cifrar 100 MB de texto plano cuesta
~40-55 ms por nota en el intérprete, más el PUT (~12 ms). 1000 × ~69 ms = los 70 s.
**Próximo corte**: WebCrypto (`SubtleCrypto`) en el navegador con fallback a noble en
Node — el provider pasa a asíncrono y el camino de hash/cifrado baja a unidades de ms.

**T4** (36,5 s) sigue dominado por el `writeTemp` por archivo contra el puente (base64 de
133 KB por POST). La solución es del mismo tipo: un `writeMany` por lotes.

Los ~1,9 s de T2/T3 son **el arranque de Chromium del CLI**: el ciclo neto (~0,3-0,4 s)
está por debajo del target de un segundo, y el plugin (motor persistente) no paga arranque.

## Cómo se leen

- **T1 está dominado por el round-trip POR LOTE, no por los bytes.** El push manda **1 op
  por lote**: 1000 altas = 1000 lotes secuenciales (~99 ms cada uno: `PUT` del blob por CAS
  + firma + batch por WS + ack). Es el mismo techo que tiene pendiente el «restore masivo»
  (agrupar N ops por lote). Con lotes agrupados, T1 debería acercarse al costo de T4.
- **T2/T3 son el costo del scan por hash**: el ciclo releé y hashea los 100 MB del vault en
  cada pasada (~8,6 s ⇒ ~12 MB/s de hasheo+comparación en este host). No hay caché por mtime
  a propósito: el hash es la verdad.
- **T4 es la bajada real**: páginas de hasta 100 revisiones por pull, descifrado AEAD y
  escritura al disco, ~39 ms por revisión. Bajar es ~2,5× más rápido que subir.

## Reproductibilidad

Los números de arriba son de UNA corrida en el lab de desarrollo; sirven como orden de
magnitud y como referencia de regresión (comparar antes/después de tocar el motor o el
server). Al publicar números nuevos: anotar fecha, versión del server, `N`, `KB` y si el
cliente es local (loopback) o remoto.

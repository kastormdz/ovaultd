## 🔴 Rompe algo

- **El ACK no resuelve concurrencia (§ 4.1, § 4.4).** `push` no lleva una precondición como `expected_rev`: dos clientes pueden sobrescribirse aunque ambos esperen ACK. Hace falta compare-and-swap transaccional. Tampoco `(path_id, hash)` identifica una operación: restaurar un archivo borrado con idéntico contenido debe producir un cambio. Usá `operation_id` para reintentos.

- **La corrección multiarchivo puede perder eventos (§ 3, § 4.3, § 17.1).** Ahora varias filas comparten `rev`; si `limit` corta una revisión y el cliente avanza `since`, omite las restantes. Definí límites de batch y ACK únicamente tras aplicarlo completo. Además, retención/compaction exigen un cursor mínimo y resincronización completa para clientes demasiado atrasados: “nunca listar” es incompatible.

- **El esquema no permite reconstruir lo prometido (§ 3, § 4.3–4.4, § 5.1).** Faltan `T` en tablas/mensajes y `path_enc` en bootstrap e historial; tampoco se preserva suficiente metadata histórica de renames. Un dispositivo nuevo no puede recuperar nombres desde HMACs. `restore` viola el `CHECK` de operaciones. Distinguí explícitamente hash local del plaintext y `blob_id` del ciphertext.

- **El MAC separado no está mal; el binding sigue incompleto (§ 5.1, § 17.1).** Hay que verificar `HMAC(k_path, path_descifrado) == path_id`, o autenticar esa asociación: cambiar `path_enc` podría presentar contenido bajo otro nombre sin invalidar `T`. Tampoco se autentican borrados ni épocas de clave. Un servidor puede reproducir un par histórico válido bajo el mismo path. Necesitás registros autenticados y checkpoints cliente contra rollback; el MAC propuesto no alcanza.

- **El challenge-response tiene un enrolamiento indefinido (§ 4.3–4.4, § 5, § 17.1).** Con solo `H(k_keyhash)`, el servidor no verifica `HMAC(k_keyhash, …)`; almacenando `k_keyhash`, tiene una credencial equivalente para generar proofs. Eso no revela la master, pero contradice “nunca viaja crudo” y exige definir cómo llega al servidor. Tampoco cierra G-02, referido a confirmación de contenido.

- **La rotación no está completa (§ 5.2, § 17.1).** Cambiar master cambia también `path_id` y las claves de nombres/MACs; recifrar cuerpos no migra el índice. Faltan épocas por registro, transición atómica y claves para leer historia retenida. Separá clave aleatoria del vault de sus envolturas por password/recovery. La recovery necesita entropía especificada: base32 es una codificación, no seguridad. “Invalidarla” no revoca copias anteriores ni garantiza uso único.

- **El GC todavía puede destruir snapshots (§ 3, § 7, § 17.1).** El conteo corregido omite referencias desde snapshots. La transacción SQL tampoco incluye el filesystem/S3: faltan staging, publicación durable y coordinación entre GC y nuevas referencias. `gc --verify` sin exclusión o protección temporal tiene carreras. Los backups necesitan un corte coherente DB/CAS.

- **La reconciliación puede pisar cambios (§ 4.2, § 6, § 15.3).** Aplicar remoto antes de preservar modificaciones locales es peligroso; “ambos existen” no significa conflicto: compará contra la base sincronizada. Un tombstone sin retirar el archivo visible no implementa borrado. RF-35 tampoco distingue placeholders de borrados legítimos offline. Finalmente, reemplazar diff3 por diff-match-patch no proporciona automáticamente un merge de tres vías: falta el algoritmo de solapamientos.

- **El arreglo del retrofit es contradictorio (§ 8, § 11, § 17.1).** Un `CryptoProvider` passthrough escribe plaintext. No satisface “nunca”, aunque exista la interfaz. Binding, atomicidad y autenticación tampoco pueden quedar para fase 12, después de publicar. Deben funcionar antes del primer sync persistente.

## 🟡 Importante

- **MLE tiene garantías exageradas (§ 5.1, § 17.3).** Repetir exactamente mensaje y AAD con ese nonce no rompe automáticamente XChaCha. Pero el esquema requiere revisión criptográfica, separación de `k_nonce` y formato/límites propios para AES-GCM. El keyed hash evita confirmación offline sin clave, no cuando el atacante induce una subida candidata. Rechazar postergar dedupe es injustificado: CAS funciona con ciphertext aleatorio, aunque pierda deduplicación automática.

- **Reanudación no es cifrado streaming ni delta sync (§ 5, § 8, § 13).** El nonce depende del plaintext completo: necesitás dos pasadas o buffering. La API AEAD de archivo completo no garantiza memoria acotada. Tampoco CAS por archivo evita retransmitirlo al cambiar una línea. Definí chunks autenticados y manifiesto, o retirale esa promesa al MVP.

- **Faltan reglas de confianza y downgrade (§ 4.4, § 5, § 10.4).** Exigí TLS validado también para el `host` devuelto, instalación documentada de CA propia y autorización por vault al descargar blobs. Fijá suites/versiones mínimas y límites del KDF. `proto:1` no basta. El nonce aleatorio de `path_enc` tampoco elimina igualdad ya expuesta por `path_id`; documentá tamaños, tiempos y relaciones observables.

- **El nivel A alcanza; su justificación y migración no (§ 10.2–10.4).** Solo dos hashes ya suman 64 bytes, no 40; faltan nombres cifrados, índices e historial. E2E no inutiliza concurrencia, réplicas ni herramientas operativas de PostgreSQL. SQLite+CAS local alcanza para el objetivo inicial, con backup consistente y restore probado. Implementá solo A: dos backends desde día uno y particionado son trabajo prematuro. S3 no exige multinodo; migrar datos no es cambiar variables. El proxy/TLS preexistente desmiente la instalación autosuficiente de un paso.

- **La fase 2 va a explotar (§ 11).** Una semana incluye el núcleo durable offline, renames, concurrencia y recuperación tras crashes. La tabla ya suma aproximadamente **9,5–15 semanas para el MVP**, no 6–8; el total ronda **20–30**, sin margen. Cripto streaming y merge agregan incertidumbre, no tareas mecánicas. Adelantá backup/restore y pruebas de fallos; postergá Yjs, múltiples backends y API de lectura.

- **Persisten huecos de producto frente al oficial (§ 14, § 15).** No hay requisitos explícitos para retirar colaboradores ni abandonar una membresía compartida: desvincular un dispositivo no equivale a salir del vault. Tampoco se exige previsualizar versiones de notas antes de restaurarlas. ACL e historial genéricos no especifican esas capacidades.

- **Publicabilidad no equivale a “sin WASM” (§ 16, § 17.1).** WASM no es ofuscación por definición; prohibirlo puede perjudicar el KDF móvil sin asegurar aprobación. Una attestation acredita procedencia, no demuestra reproducibilidad ni aprobación. Las restricciones sobre forks tampoco prohíben toda reutilización de componentes licenciados. Fijá commits de las políticas citadas y verificá portal/workflow vigentes antes de convertirlos en criterios contractuales.

## 🟢 Menor

- **El triage no derogó requisitos anteriores (§ 14–15, § 17).** Siguen RF-25 con AAD, RNF-04 con KDF antiguo y requisitos de planes/regiones contradictorios. Marcá explícitamente cuáles quedan sustituidos; el implementador no debería inferir precedencias.

## Top 10 correcciones

1. Agregá `expected_rev` y `operation_id` con validación transaccional (§ 4.4).
2. Definí batches indivisibles y recuperación de cursores vencidos (§ 4.3, § 7).
3. Completá esquema y mensajes con nombres, MACs, épocas y metadata histórica (§ 3).
4. Autenticá paths y operaciones; incorporá protección contra rollback (§ 5.1).
5. Cerrá suite criptográfica, enrolamiento y formato recovery con vectores auditables (§ 5).
6. Diseñá rotación atómica de cuerpos, índices y claves históricas (§ 5.2).
7. Protegé snapshots, uploads y backups frente al GC concurrente (§ 7).
8. Especificá reconciliación contra base, borrado efectivo y merge verdadero (§ 4.2, § 6).
9. Eliminá passthrough y reestimá después de spikes y tests de crashes (§ 11).
10. Entregá solo nivel A inicialmente y verificá políticas con fuentes versionadas (§ 10, § 16).

---
_revisor: gpt-6-astra · effort=high · 216s · tokens: in=22360 out=9931 (reasoning=7949)_

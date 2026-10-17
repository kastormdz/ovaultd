Acá tenés la revisión crítica de la especificación. Hay contradicciones severas en la interacción entre criptografía y el modelo de datos, y varios puntos donde arrastraste la lógica de negocio de un SaaS a una herramienta que está diseñada para ser self-hosted.

### 1. Errores técnicos y contradicciones internas

🔴 **Contradicción destructiva entre AAD, CAS y Renames (§ 3, § 5, § 6 vs. G-01/RF-25):**
Exigís que el servidor deduplique los blobs según el `hash` del ciphertext (CAS, § 5) y afirmás que un rename es solo "metadata" y no requiere re-subida si el hash del contenido no cambia (§ 6). Al mismo tiempo, en RF-25 obligás a que el `aad` (Additional Authenticated Data) incluya el `path_id`. Al inyectar el `path_id` en la autenticación del cifrado, el MAC resultante al final del archivo cambia para cada ruta. Consecuencia: dos archivos idénticos en distintas rutas tendrán ciphertexts distintos, lo que destruye totalmente la deduplicación del CAS. Peor aún, al renombrar un archivo cambia su `path_id`, lo que invalida el MAC anterior y te obliga a re-cifrar y resubir el archivo completo, rompiendo lo que prometiste en § 6.

🔴 **Merge destructivo con diff3 en texto plano (§ 6):**
Establecés el motor `diff3` para el merge de tres vías en archivos `.md`. Markdown suele usarse con párrafos larguísimos tipeados en una sola línea física (soft wraps). `diff3` trabaja estrictamente a nivel de línea entera; cualquier edición concurrente en un mismo párrafo va a fallar y generar un conflicto irresoluble. Necesitás sí o sí un algoritmo basado en caracteres/palabras como `diff-match-patch`, que es lo que inteligentemente usa el servicio oficial.

🟡 **Doble transporte de datos innecesario y roto (§ 4.3 vs. § 4.4):**
Definís la subida CAS reanudable vía HTTP REST (`POST /blobs` usando `tus` o `Content-Range`) en § 4.3, pero en § 4.4 también diseñás frames binarios multiplexados (`blob_head`, `push`) mezclados sobre WebSocket. Implementar y mantener ambas vías de transferencia es redundancia pura y un dolor de cabeza. Además, los frames WS de ejemplo que pusiste no incluyen offsets, por lo que la reanudación por caída de conexión sobre WS no funcionaría de todas formas.

### 2. Criptografía

🔴 **Cifrado no determinista anula el CAS (§ 5):**
Ordenás cifrar los blobs antes de enviarlos y calcular el hash para el CAS. Sin embargo, no definís cómo se deriva el nonce de inicialización para `XChaCha20-Poly1305`. Si generás un nonce aleatorio o basado en el reloj (el estándar), guardar el mismo contenido dos veces generará ciphertexts completamente distintos. Para que un CAS deduplique sobre cifrado, necesitás *Message-Locked Encryption* (por ejemplo, derivar el `nonce = hash(plaintext)`). Sin esto detallado, el almacén de blobs jamás va a deduplicar nada.

🟡 **Falta de estrategia de rotación de claves:**
El diseño no contempla qué sucede si se compromete la recovery key o el usuario necesita rotar su password por seguridad. No hay ningún mecanismo ni endpoint para regenerar la master key y re-cifrar los archivos (un *re-keying* del vault). Ante una fuga local, el vault queda quemado.

### 3. Huecos frente al servicio oficial

🟡 **Enlaces simbólicos (symlinks):**
La spec no menciona cómo reacciona el `FileAdapter` (§ 8) frente a symlinks en el disco. En entornos de escritorio (especialmente Linux), escanear symlinks alegremente suele generar bucles infinitos en el watcher y explosiones masivas de duplicación hacia el servidor. El cliente oficial tiene reglas rígidas para ignorarlos.

🟢 **Limpieza de almacenamiento local huérfano:**
Si el usuario apaga el plugin, desvincula el device o desinstala la app, los gigas de la base de datos local y los blobs parciales cacheados en IndexedDB (§ 8) van a quedar huérfanos, comiéndose el almacenamiento del sistema sin que el usuario lo sepa. Te falta un hook de cleanup local.

### 4. Sobre-ingeniería (Infraestructura y Modelo)

🔴 **Cuotas y planes de SaaS en un producto Self-Hosted (§ 3, RF-21):**
Arrastraste los límites comerciales de la empresa (`plan`, `storage_quota` de 1 GB, tiers de límite de archivo de 5 MB/200 MB) al DDL de un proyecto puramente self-hosted. Obligar a un usuario que despliega esto en su propio NAS de 10 TB a lidiar con advertencias de "Quota exceeded" y bloqueos por subir un PDF de 6 MB es ridículo y agrega una complejidad transaccional (quota enforcer) que nadie te pidió.

🟡 **Complejidad Multi-región día cero (§ 3, § 4.3, RF-02):**
Meter columnas de `region` en la tabla de vaults yendpoints de `/vault/regions` en la Fase 1 para una herramienta que el 99% de tus usuarios va a correr en un único contenedor de Docker Compose dentro de su casa es agregar ruido, código muerto y acoplamiento inútil.

### 5. Riesgos de ejecución y estimaciones

🔴 **Tiempos de ciencia ficción en Fase 3 y 7 (§ 11):**
- **Fase 3 (Plugin Desktop, 2-3 semanas):** Pretender escribir desde cero un watcher robusto asincrónico, un motor de diff por hash, la indexación en IndexedDB, una cola de WS y toda la UI en 3 semanas para un solo dev trabajando part-time es garantizar el fracaso del cronograma.
- **Fase 7 (Móvil, 2 semanas):** En iOS el sistema operativo te mata el proceso en background rapidísimo. Ejecutar operaciones criptográficas pesadas mediante WASM puenteado dentro del WebView móvil de Obsidian, y mantener streaming de adjuntos enormes sin quebrar el límite de memoria (OOM), es un esfuerzo titánico que te va a llevar meses de prueba y error, no dos semanas.

### 6. Publicabilidad (Directorio Oficial)

🟡 **Auditoría paranoica de binarios WASM (§ 16.6):**
Al requerir algoritmos como `scrypt` y `XChaCha20` vas a tener que inyectar librerías WASM precompiladas (libsodium o hash-wasm), ya que WebCrypto no los soporta. El directorio revisa los `.wasm` con lupa extrema para prevenir troyanos. Vas a tener que justificar muy bien su inclusión y garantizar que el build CI reproduzca exactamente el binario WASM publicado, o te van a rebotar el plugin entero por violar la regla de código opaco.

---

### Top 10 correcciones

1. Definir explícitamente *Message-Locked Encryption* (nonce derivado del plaintext) para `XChaCha20-Poly1305` y así habilitar la deduplicación.
2. Eliminar el `path_id` del `aad` en los blobs para que un rename local no te obligue a re-cifrar y resubir el archivo de nuevo.
3. Reemplazar `diff3` por `diff-match-patch` para asegurar que el merge automático de texto sirva para los soft-wraps de Markdown.
4. Eliminar toda la lógica de regiones (`/vault/regions`, tablas asociadas) para ajustarse a la realidad de la arquitectura self-hosted.
5. Unificar el transporte de blobs grandes eliminándolos de los frames WebSocket y dejando exclusivamente la subida reanudable vía HTTP POST.
6. Borrar las cuotas artificiales de almacenamiento y los límites duros por tier de 5 MB/200 MB; hacerlos ilimitados o simples variables globales.
7. Añadir comportamiento defensivo explícito en el FileAdapter para detectar y saltear symlinks locales.
8. Multiplicar las estimaciones temporales de las Fases 3, 5 y 7 al menos por tres para evitar que el cronograma colapse de entrada.
9. Crear un mecanismo o comando visible en la UI para la purga forzada y limpieza total de la base local de IndexedDB.
10. Añadir un apartado específico en la sección de publicación detallando el método de build reproducible y verificación del binario WASM.

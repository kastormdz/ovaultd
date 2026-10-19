# Checklist manual — plugin de escritorio (`oVaultd Sync`)

Prueba de la UI del plugin en Obsidian de **escritorio** contra un server real. No
reemplaza a los E2E (los specs de Chromium y los E2E de contenedor cubren el motor y el
wire): acá se mira lo que un test no ve — la UX, los modales, los mensajes, y que nada se
pierda con las manos en el teclado.

- **Requiere**: un server `ovaultd` accesible (el dominio propio, p. ej.
  `https://sync.example.com`), una cuenta con acceso y un vault de PRUEBA. Los pasos §6 y
  §8 son destructivos sobre el estado del vault: no usar el vault de trabajo.
- **Anotar antes de empezar**: versión del plugin (Ajustes → Complementos → oVaultd Sync)
  y versión del server (`GET /api/v1/meta` → `server_version`).

## 1. Cuenta y alta del vault

- [ ] Ajustes → oVaultd Sync → sección **Cuenta**: servidor + email + password de CUENTA →
      **Entrar**. Esperado: «sesión iniciada» y aparece **«Vaults de tu cuenta»**.
- [ ] Con la sesión, **Crear vault…** (nombre + password de cifrado INVENTADA acá). Esperado:
      modal con la **recovery key** (copiable), una sola vez; el motor arranca y sube la
      carpeta entera como altas.
- [ ] La lista de vaults muestra el creado con ✓ y **[Vincular…] / [Recuperar acceso…] /
      [Borrar…]**. **Borrar…** pide el NOMBRE exacto del vault (no un booleano).

## 2. Motor y primer ciclo

- [ ] La barra de estado muestra la fase (arrancando → al-día). Esperado: «al-dia —
      conectado a <server>» sin errores en `motor.log` (`.obsidian/plugins/ovaultd/`).
- [ ] Tocar cualquier nota y esperar un ciclo. Esperado: «↑N subidas» y luego «sin cambios».
- [ ] Cortar la red 10 s y recargar el plugin. Esperado: el motor se recupera SOLO
      (reintento de arranque a los 30 s); no queda muerto esperando un toggle.

## 3. Dos dispositivos (requiere un segundo equipo/móvil en el MISMO vault)

- [ ] Editar una nota en A. Esperado en B: llega en **< 2 s** (aviso en vivo) o a los ~30 s
      (rescate). El contenido es idéntico (md5).
- [ ] Renombrar un archivo en A. Esperado en B: borrado+alta (así llegan los renames
      externos); nada queda duplicado.
- [ ] En el teléfono: los comandos de escritorio (export/import) avisan que no están
      disponibles; la **historia** funciona igual.

## 4. Config portable (themes / apariencia)

- [ ] Elegir otro theme y tocar la apariencia (fuente/tamaño) en A.
      Esperado en B: `themes/*` + `appearance.json` bajan y se aplican.
- [ ] Togglear un plugin en A. Esperado: **`community-plugins.json` NO viaja** (es
      por-dispositivo) y **no hay conflicto** por esa lista.
- [ ] Actualizar el plugin en A. Esperado: el manifest del PROPIO plugin **no** viaja; B
      no «miente» su versión.

## 5. Conflicto real y resolución

- [ ] Con A desvinculado (o su motor detenido) editar la MISMA nota en A y subir otra cosa
      desde B sobre esa nota. Re-vincular A.
      Esperado: A marca el conflicto (status bar / aviso); la nota NO se pisa sola.
- [ ] **Resolver conflicto…** → elegir **local** → la nota queda local y el push sube una
      revisión nueva. Repetir con **remoto** → la nota queda como el server.
      Esperado: en ambos casos nada se pierde y el motor vuelve a «al-dia».

## 6. Merge 3-vías (ediciones disjuntas)

- [ ] Con el motor de B detenido, editar zonas DISTINTAS de la misma nota en A y B.
      Reactivar el motor de B. Esperado: fusión automática (las dos ediciones presentes);
      sin conflicto. Con ediciones SOLAPADAS: conflicto (conservar ambas versiones).

## 7. Historia (la máquina del tiempo)

- [ ] Abrir la historia de una nota (comando/`HistoryView`). Esperado: revisiones con
      fecha, op y tamaño, la nueva primero.
- [ ] **Preview** de una revisión vieja. Esperado: se ve el contenido VIEJO; el archivo del
      vault no cambia.
- [ ] **Restaurar** esa revisión. Esperado: el archivo queda con la versión vieja y el sync
      sube una revisión NUEVA (la historia no se reescribe ni se pierde nada).

## 8. Export / import

- [ ] **Exportar el vault remoto a una carpeta…** (escritorio). Esperado: archivos con md5
      idénticos a los del vault; los conflictuados bajan la versión DEL SERVER (correcto).
- [ ] **Importar una carpeta al vault…** sobre un vault vacío. Esperado: nada existente se
      pisa; dotfiles y excluidos quedan afuera; el ciclo siguiente los sube.

## 9. Llaves del vault

- [ ] **Cambiar password…** (valida la actual y re-sella). Esperado: el vínculo local sigue
      andando; la password NUEVA abre el material.
- [ ] **Recuperar acceso…** con la recovery key. Esperado: re-sellado + vínculo; con la
      password perdida es la única puerta.

## 10. Desvincular y volver

- [ ] **Desvincular** (confirmación): borra el vínculo Y el índice durable. Esperado: la
      cuenta SOBREVIVE (no hay que loguearse de nuevo).
- [ ] **Vincular** de nuevo la misma carpeta. Esperado: sube el vault entero como altas en el
      primer ciclo (el índice viejo NO sobrevive: si dijera «sin cambios», el desvínculo no
      borró el índice — bug).
- [ ] Con el vault de A desvinculado, mirar el de B: nada cambió (desvincular no toca el
      server).

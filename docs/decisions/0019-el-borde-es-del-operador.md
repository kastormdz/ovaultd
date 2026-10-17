# 0019 — El borde es del operador: sin proxy dentro del repo/docker

- Estado: aceptada
- Fecha: 2026-10-17

## Contexto

El stack traía un `docker/proxy/nginx.conf.example` montado por el overlay de desarrollo
(`:8888`) y el container-E2E probaba a través de él (host explícito para `/access`, upgrade
de WS, `client_max_body_size`). Sirvió para dejar probado que el server vive bien detrás de
un proxy reverso. Pero en el entorno real del operador el borde ya existe: **NPM Plus**, con
TLS y sus propias reglas. Mantener un nginx adentro del repo duplica responsabilidades y
confunde qué hay que deployar.

## Decisión

- El repositorio y la imagen **no traen proxy**: se elimina `docker/proxy/` y el servicio
  `proxy` del overlay de desarrollo. El `compose.yaml` publica en loopback por defecto
  (`OVAULTD_ADDR`), que es exactamente lo que un borde local necesita.
- El TLS y la terminación HTTP los hace el operador (NPM Plus en producción). Los tres
  puntos que el proxy DEBE respetar quedan documentados en `docs/deploy.md` con la
  configuración exacta para NPM Plus (pestaña *Advanced* del proxy host), además del
  snippet nginx genérico para otros bordes:
  1. `Host` explícito hacia el upstream (`/access` lo usa para la URL de subida).
  2. Upgrade de WebSocket (NPM Plus lo maneja de fábrica; el snippet lo explicita).
  3. `client_max_body_size` ≥ 40m y buffering off donde corresponda.
- `tests/scripts/container-e2e.sh` pasa a probar **directo** contra el server (HTTP + WS +
  cripto real, mismos gestos, sin nginx). La validación "detrás del borde" es un smoke
  manual documentado contra NPM Plus real (el operador ya lo corre).
- `docs/deploy.md` reordena: NPM Plus primero (camino oficial del operador), nginx genérico
  como ejemplo alternativo.

## Consecuencias

- Una pieza menos en el stack de desarrollo; el E2E deja de depender de un nginx del repo y
  prueba lo que el server garantiza de verdad.
- La superficie "detrás de proxy" queda como responsabilidad explícita del operador, con la
  config exacta documentada — si el borde rompe algo (WS, subidas grandes), la doc tiene los
  tres puntos a revisar.
- El overlay de dev sigue existiendo solo para construir la imagen local.

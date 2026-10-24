-- Versión del cliente (plugin de Obsidian / CLI) que reportó su último handshake `hello`.
-- Opcional: un cliente viejo no la manda y la columna queda NULL (el panel muestra "—").
-- Sirve para saber qué versión corre cada device sin entrar a mirar cada máquina.
ALTER TABLE devices ADD COLUMN app_version TEXT;

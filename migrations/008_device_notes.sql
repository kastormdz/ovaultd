-- Notas reales del vault, reportadas por el CLIENTE.
-- El server no puede derivarlas: el path viaja cifrado (path_enc) y el E2EE no le permite
-- distinguir una nota `.md` de un adjunto. `files` sigue siendo la fuente exacta de archivos.
ALTER TABLE devices ADD COLUMN notes INTEGER;

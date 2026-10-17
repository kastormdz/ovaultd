-- 002_auth_challenges.sql — challenges de autenticación (el plan lo anticipó: "002, únicamente si hace falta")
--
-- Por qué hace falta: el challenge tiene que ser de UN SOLO USO y con vencimiento. Guardarlo en memoria del proceso
-- funciona hasta el primer reinicio (y rompe con más de una instancia): el cliente recibiría un 401 "challenge
-- inválido" por un reinicio, no por un ataque. La tabla es la que hace que el rechazo del replay sea una propiedad
-- del sistema y no del tiempo de actividad del proceso.

CREATE TABLE auth_challenges (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  device_id  INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
  vault_id   INTEGER NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
  challenge  BLOB NOT NULL,                 -- 32 bytes del CSPRNG (nunca derivados de nada adivinable)
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  expires_at TEXT NOT NULL,
  used_at    TEXT                           -- NULL = sin usar. El UPDATE condicional es lo que lo hace de un solo uso
);

-- Búsqueda del challenge que trae el cliente: (device, vault) primero, que es lo que además ata el challenge a ESE
-- dispositivo y a ESE vault (un challenge emitido para un vault no sirve para otro).
CREATE INDEX idx_challenges_lookup ON auth_challenges(device_id, vault_id, challenge);

-- Un challenge pendiente no puede repetirse: si dos emisiones devolvieran el mismo valor, el replay sería trivial.
CREATE UNIQUE INDEX idx_challenges_unico ON auth_challenges(device_id, vault_id, challenge) WHERE used_at IS NULL;

-- Limpieza de vencidos: no se borran solos, pero el índice hace barato el barrido.
CREATE INDEX idx_challenges_expira ON auth_challenges(expires_at);

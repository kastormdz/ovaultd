# 0018 — Versionado y compatibilidad entre server y plugins

- Estado: aceptada
- Fecha: 2026-10-17

## Contexto

Hoy el server es `0.1.0`, el plugin es `0.1.0` y el runtime del wire lleva su propia versión
(`RUNTIME_VERSION`), sin ningún lazo declarado entre ellos: nada dice si un plugin viejo
habla con un server nuevo. Tampoco hay workflows ni releases en GitHub: distribuir es
`docker compose up` con una imagen local o un checkout. El pedido del operador: numerar
server y plugin, que se sepa cuándo son compatibles, y poder publicar releases.

## Decisión

### Semver por artefacto, release train por pareja

- `ovaultd` (Rust, `Cargo.toml`) y `oVaultd Sync` (`manifest.json`) versionan con semver.
- **Release train**: mismo `MAJOR.MINOR` en ambos = pareja probada junta (los patches son
  independientes). Ej.: server `0.2.x` ↔ plugin `0.2.x`. El wire (`sync2`/protocolo v2) NO
  cambia con esto: su versión sigue siendo la del formato.

### Contrato de compatibilidad

- `GET /api/v1/meta` (público, sin auth) devuelve como mínimo:

  ```json
  {
    "server_version": "0.2.0",
    "api": "v1",
    "wire_protocol": "v2",
    "min_plugin_version": "0.2.0",
    "features": ["admin-web", "meta", "…"]
  }
  ```

- Todo cliente identifica su versión por header `x-ovaultd-client: oVaultd-plugin/0.2.0`
  (el registro de devices ya guarda `platform`; el header es para logs y diagnóstico).
- El plugin, al vincular y al conectar, consulta `/meta` y decide:
  - versión propia < `min_plugin_version` → **no vincula**, con mensaje claro ("actualizá el
    plugin"); el server no es el culpable si el plugin es viejo.
  - versión propia con MINOR mayor que el server → aviso suave (features pueden faltar).
- `/healthz` sigue siendo el probe tonto (sin auth, sin datos); `/meta` es el contrato.

### Releases en GitHub

- Tags por artefacto: el server usa `server-v0.2.0` en su repo. El plugin usa **la versión
  pelada** (`0.2.0`) porque el directorio de la comunidad de Obsidian descarga
  `main.js`/`manifest.json`/`styles.css` del release cuyo tag coincide exactamente con el
  `version` del manifest — y esos tres archivos van **sueltos** como adjuntos binarios, más
  una atestación de provenance (`actions/attest`). El preflight verifica tag ↔ `manifest.json`
  ↔ `package.json` ↔ `versions.json`.
- Workflows (nuevos, `.github/` está vacío hoy):
  - `release-server.yml` (on `server-v*`): gates completos (cargo test) → binario
    `x86_64-unknown-linux-musl` + tar.gz + SHA-256 → imagen `ghcr.io/kastormdz/ovaultd`
    (`X.Y.Z` + `latest`) → GitHub Release con los assets.
  - `release-plugin.yml` (on `plugin-v*`): `npm ci` + tests + build → zip
    (`main.js`, `manifest.json`, `styles.css`) → GitHub Release (formato listo para BRAT y
    para la tienda de Obsidian, que exige un release por versión).
- `CHANGELOG.md` por repo (Keep a Changelog), armado a mano en el corte de release.
- El README de cada repo gana la sección "Releases" con la matriz de compatibilidad vigente
  (server ↔ plugin ↔ wire).

## Consecuencias

- Un operador sabe exactamente qué bajar y qué combinar; el check de `/meta` evita el caso
  silencioso "plugin viejo contra server nuevo" (que hoy solo se descubre a los errores).
- Publicar es taguear: pasa por los mismos gates que CI local (los workflows corren lo mismo
  que los scripts de test que ya existen).
- La imagen `ghcr.io` queda pública (igual que los repos, AGPL) — el digest se cita en
  `compose.yaml` cuando toque la distribución por imagen.
- BRAT/tienda quedan desbloqueados para el plugin sin trabajo extra.

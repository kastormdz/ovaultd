# ovaultd CLI

The non-editor client: it links a folder to a remote vault, keeps it in sync, and gives
an agent plain filesystem access to the decrypted vault. It is the "device-agent" from
the plan: it holds the passphrase and decrypts **locally** — the server never sees
plaintext (E2EE).

## Install the `ovaultd` command

The package is private (not published to npm). The command is a shim over the compiled
`dist`; there is no standalone binary (the sync runtime drives Chromium via Playwright,
so a self-contained binary would still need the browser).

```bash
npm run build -w @ovaultd/cli
bash scripts/instalar-cli.sh                 # → ~/.local/bin/ovaultd
OVAULTD_BIN_DIR=/usr/local/bin bash scripts/instalar-cli.sh
```

Requires `node` in `PATH`. The shim points at **this checkout**: re-run the script after
moving the repository.

## First link (new device)

```bash
ovaultd login https://obs.example.com --email you@example.com
ovaultd list-remote
ovaultd setup <ID> --dir ~/Obsidian
ovaultd sync <ID>
```

- `login` stores a device token (`0600`) and registers the device with the machine's
  hostname as its name; the **account** password is never stored.
- `setup` verifies the **vault** password against the wrapped key and links the folder.
- The first `sync` pulls the whole history into the folder.

## Everyday sync

```bash
ovaultd sync                 # one pass over every linked vault
ovaultd sync <ID>            # one vault
ovaultd sync --continuous    # keep running (systemd-friendly)
ovaultd list-local           # scan + hash-diff, no network
ovaultd status
```

## The vault password is asked once

The first command that needs the vault key (the first `sync`, `history`, `preview` or
`restore`) derives the subkeys and caches them in the local vault state
(`~/.config/ovaultd/vaults/<server>_<id>.json`, mode `0600`). Later runs — including a
systemd timer — do not ask again. The cache is invalidated when the vault key epoch
changes.

```bash
ovaultd olvidar-clave          # drop the cached subkeys (the next command asks again)
ovaultd olvidar-clave <ID>
```

The state file is private but is as sensitive as the vault itself: whoever reads it can
decrypt the vault's blobs. `unlink` deletes it together with the rest of the local state.

## Filesystem commands (agent access)

With the vault linked, these operate **locally**: no network, no password prompt, no
Chromium (the vault is resolved from the local state in ~10 ms).

| Command | What it does |
|---|---|
| `ovaultd cat <path>` | prints a file (binary-safe) |
| `ovaultd write <path> --stdin \| --file <f>` | writes a file with atomic rename |
| `ovaultd ls [prefix] [--json] [--todo]` | lists files (`.obsidian/` excluded unless `--todo`) |
| `ovaultd rm <path> [-r]` | deletes a file or directory |

Rules:

- Paths are **vault-relative** and validated with `validarRutaDeVault`: no `..`, no
  absolute paths, no symlinks escaping the vault. A rejected path exits non-zero.
- `write` never takes content from argv: use `--stdin` (multiline markdown) or `--file`.
- Every `write`/`rm` is a normal file change: the next `sync` turns it into a **signed
  revision** (chained by `expected_rev`), other devices pull it, and `history`/`restore`
  apply to it like any other revision.
- The commands resolve the vault from the local state; when more than one vault is
  linked, pass `--vault <n>`.

## History and recovery

```bash
ovaultd history <vault-path>
ovaultd preview <vault-path> --rev <n> --diff
ovaultd restore <vault-path> --rev <n>     # uploads as a NEW revision
```

## Agent flow

- **CRUD**: `ovaultd cat/write/ls/rm` over the linked folder (above).
- **Search**: keep a local indexer over the linked folder (chunk + embed). Search is a
  client-side concern by design: the server cannot index what it cannot read.
- **Sync**: a systemd timer (or `sync --continuous`) keeps the folder current, so the
  agent always works on a local, decrypted copy.

# ovaultd

Self-hosted, end-to-end encrypted file sync for Obsidian vaults. Your server, your keys: the server stores opaque, encrypted blobs and never sees what is inside them.

Functional parity with Obsidian Sync on infrastructure you own, built around a frozen wire spec (`v2`) and a crypto protocol (`sync2`) that was verified byte-for-byte between Rust and JavaScript before either side shipped. Design notes, ADRs and code comments are in Spanish; this README is the English entry point ([versión en español](README.es.md)).

## What it does

- **Encrypted sync of vault files** (add / modify / delete / move) with revisions and compare-and-swap (`expected_rev`), so racing edits become conflicts instead of silent data loss.
- **Resumable uploads** (a subset of tus) with per-account quotas; an interrupted transfer resumes from its last acknowledged offset.
- **HTTP control plane plus a WebSocket data plane** for revision deltas.
- **Encrypted paths and signed revision records** (Ed25519, chained by `expected_rev`): tampering is detectable by clients.
- **A single hardened container**: non-root uid, read-only rootfs, every capability dropped, SQLite WAL and a content-addressed blob store under `/data`.
- **Edge-ready**: the server publishes on loopback and speaks plain HTTP; `docs/deploy.md` documents exactly what the edge (Nginx Proxy Manager, nginx, Caddy) must get right. The repository ships no proxy — the edge belongs to the operator (ADR-0019).
- **Web admin panel**: accounts with roles (create, disable, promote or demote, one-shot password-reset links, account deletion with vault purge), vaults with per-vault detail and deletion, devices with one-click revocation, a paginated and filterable audit log, server settings (open/close registration), and self-service password change. Server-rendered with embedded assets, CSRF on every POST, no JavaScript required, works over the LAN or behind the edge.
- **No public registration.** The first account is created from the web at `/setup` (first run only, ADR-0016); further accounts are created by an admin from the panel or with the admin CLI inside the image (ADR-0011).
- **A full client stack**: a durable offline core (versioned state store, save-first outbox, apply journal), a CLI (`login`, `create-remote`, `setup`, `sync --continuous`, …) and an Obsidian plugin that runs the same core inside the editor — event-driven sync, explicit conflict resolution, vault keys management (recover with the recovery key, change the vault password) and vault export/import.
- **Operational tooling**: `backup.sh` / `restore.sh` with checksummed verification, crash tests with real process aborts, and an encrypted-scan canary that greps raw storage for plaintext.

Every claim above is backed by executable gates, not mocks: Rust unit and integration suites, TypeScript suites, browser durability specs against a real Chromium, a full server smoke test with live crypto and WebSocket traffic, and a container E2E that builds the real image and verifies persistence across a recreate.

## How it works

```mermaid
flowchart LR
    subgraph devices [Your devices]
        CLI["CLI + durable runtime"]
        P["Obsidian plugin"]
    end
    CLI -- "HTTPS + WSS, sync2 encrypted" --> RP["Reverse proxy (TLS ends here)"]
    P --> RP
    RP --> S["ovaultd server (Rust) — SQLite WAL + CAS"]
```

The server persists encrypted blobs, signed revision records and account metadata. Your keys are derived from your password and recovery key, and they never leave your devices. Revision records are signed by device keys and chained by `expected_rev`, so tampering is detectable by clients.

| Path | What it is |
|---|---|
| `crates/ovaultd-server` | the server (axum, tokio, sqlx; migrations embedded in the binary) |
| `crates/ovaultd-crypto` | Rust implementation of `sync2`, used in tests and the interop spike |
| `crates/ovaultd-protocol` | wire types shared by both sides |
| `packages/protocol` | the wire contract in TypeScript (types, strict base64, canonical serialization) |
| `packages/crypto` | `sync2` in TypeScript, a byte-faithful port of the spike |
| `packages/core` | durable client core: versioned IndexedDB state, save-first outbox, apply journal, sync session (WS + HTTP) and pull reconcile |
| `packages/cli` | `login`, `create-remote`, `setup`, `list-remote`, `status`, `list-local`, `sync` (+ `--continuous`), `config`, `unlink`, plus the durable Chromium runtime |
| `spikes/crypto-interop` | the Rust ⇄ JS interop spike and the golden-vector freeze tooling |
| `tests/` | browser specs, frozen fixtures, smoke / canary / container-E2E scripts |
| `scripts/` | operational tooling: `backup.sh`, `restore.sh`, the leak scanner used before every push |
| `docs/` | protocol and crypto specs, ADRs, deploy guide, runbook |
| `xtask/` | repo tasks (`cargo xtask protocol-check`, `cargo xtask backup-restore`) |

The Obsidian plugin lives in its own repository: [github.com/kastormdz/ovaultd-plugin](https://github.com/kastormdz/ovaultd-plugin). It is published in the community directory as **[oVaultd Sync](https://community.obsidian.md/plugins/ovaultd)** — installable from inside Obsidian (Settings → Community plugins → Browse).

## Security model

Everything that matters happens on your device.

- Your password is stretched with Argon2id into a KEK, and the vault key is wrapped with it (and with a recovery key that is shown exactly once).
- Content is encrypted with XChaCha20-Poly1305 under per-vault, per-content keys. The nonce is derived from the plaintext and travels inside the stored blob, so the client can decrypt it later without the server's help (ADR-0014).
- Blob IDs are SHA-256 of the stored (encrypted) bytes. Identical content deduplicates inside a vault, and because the nonce derivation is keyed, deduplication is not an external oracle.
- Paths are encrypted as well; the server keeps them as opaque IDs plus ciphertext.
- Revision records are signed (Ed25519) and chained by `expected_rev`.
- **Login is rate-limited**: 5 attempts per minute per `ip+email`, and the next one gets a `429` with `Retry-After` — attempts against non-existent accounts count too, so password spraying dies the same way. Proxy headers (`X-Forwarded-For`, `X-Real-IP`, `X-Forwarded-Proto`) are honored **only** when you declare a trusted edge (`OVAULTD_TRUST_PROXY=1`); by default the client IP is the real TCP peer, which a client cannot forge.

What the server can see: account email, device names, revision numbers, blob sizes and hashes, timing metadata. What it cannot see: file contents, file paths, keys, your password. Details in `docs/crypto.md` and `docs/decisions/`.

## Quickstart

### 1. Run the server

```bash
git clone https://github.com/kastormdz/ovaultd && cd ovaultd
cp .env.example .env

# builds the image locally
docker compose -f compose.yaml -f docker/compose.dev.yaml up -d --build

curl --fail http://127.0.0.1:8080/healthz   # direct to the server
```

To run a published image later, set `OVAULTD_IMAGE` to a real `repository@sha256:...` digest and drop the dev overlay: `docker compose -f compose.yaml up -d`.

### 2. Create your account

The **first** account is created in the browser and it is born **admin**:

![First account setup](docs/img/setup-primera-cuenta.png)

Open `http://127.0.0.1:8080/setup` and pick an email and a password. That page exists only while the server has zero accounts and disables itself the moment one exists — there is no public registration (ADR-0011/0016). If the server may be reachable before you finish this step, set `OVAULTD_SETUP_TOKEN` and the page will additionally require `?token=...`.

That account's home is the **web panel** at `/admin`:

![Panel](docs/img/panel-dashboard.png)

**Every further account** is created from the panel too — no admin tooling required. An admin opens **Cuentas**, fills in an email, an initial password and the role, and hands the password over (there is no email delivery):

![Creating an account](docs/img/panel-cuentas.png)

From that same panel an admin changes roles, disables an account, resets its password (one-time link) or deletes it, and everything that happens lands in **Auditoría**. Note that a second laptop is **not** a new account: it is another **device** of the same account, and it joins by signing in from there. `ovaultd-admin` still exists inside the image as the break-glass path (create an account, `health`, `inspect`) for when there is no browser at hand.

<sub>Screenshots come from a throwaway demo server: every account there is `@example.com`.</sub>

### 3. Your first vault

Open the plugin's settings (**oVaultd Sync**), sign in with your account, and create the vault from there: a name and a **vault password you invent**. The recovery key is shown exactly once — store it somewhere safe. That's the whole flow: the plugin runs the same crypto core as everything else, and the server never sees your password or your notes.

#### Headless and automation

The repository also ships a **CLI** (`packages/cli`) that drives the same core, for servers without a desktop and for scripts. It's the client the end-to-end tests use to talk to a server, and it's documented here rather than in the quickstart because the plugin is the intended way in:

```bash
npm install
npm run build

printf '%s\n' 'a-long-password' | node packages/cli/dist/main.js \
  login http://127.0.0.1:8080 --email you@example.com --password-stdin

node packages/cli/dist/main.js create-remote my-vault
# prints the recovery key ONCE — store it somewhere safe.
# For scripts: --recovery-out <file> writes it 0600 instead.

node packages/cli/dist/main.js status
```

`create-remote` generates the vault material. `setup` links a local folder to an existing remote vault and checks your password against the material. `list-remote` shows what the account owns.

## Behind a reverse proxy

The container publishes on loopback and speaks plain HTTP; TLS belongs to the proxy. Four things a proxy has to get right (examples for nginx, Nginx Proxy Manager and Caddy in `docs/deploy.md`):

1. **Preserve `Host`.** Clients learn the server host from `POST /access`; a rewritten Host makes them store an unreachable URL.
2. **Allow WebSocket upgrades** on `/api/v1/ws`, with long read timeouts.
3. **Raise the body limit to 40 MB and disable request buffering.** Resumable upload blocks go up to 32 MiB, and the nginx default of 1 MB kills them with a 413.
4. **Declare the edge with `OVAULTD_TRUST_PROXY=1`** so the server honors the identity headers the proxy sets (client IP for the rate limit and the audit trail, `Secure` on the panel cookie). Keep it off when the server is exposed directly.

## Compatibility

Server and plugin travel on a **release train**: the same `MAJOR.MINOR` on both sides is a pair that was tested together. Patches are independent, so `0.2.3` of the server works with `0.2.13` of the plugin.

| Server | Plugin | Wire |
|---|---|---|
| `0.2.x` | `0.2.x` | `v2` |

`GET /api/v1/meta` is the contract behind that table, and it is public on purpose: every client reads it **before** linking anything and gets the server version, the wire version, the minimum plugin it accepts, and the feature list. A plugin older than `min_plugin_version` refuses to link (with a message that points at the plugin, not at the server), and an older wire version refuses too — nothing is half-written in either case.

## Releases

Releases are cut by tag. `server-vX.Y.Z` runs the full gates — TypeScript suites, Rust suites, the server smoke test (live crypto and WebSocket) and the encryption canary — then builds a static `x86_64-unknown-linux-musl` binary plus `ovaultd-admin`, publishes the container image to GHCR (`X.Y.Z` and `latest`), and attaches the tarball with its `SHA256SUMS` to the GitHub Release. The tag *is* the declared version: the workflow fails if `Cargo.toml` disagrees, before publishing anything.

```bash
tar xzf ovaultd-0.2.3-x86_64-unknown-linux-musl.tar.gz
sha256sum -c SHA256SUMS
OVAULTD_BIND=127.0.0.1:8080 OVAULTD_DB=./datos/metadata.db OVAULTD_CAS=./datos/cas ./ovaultd
```

Or the image, which is the deployment path the project targets:

```bash
docker run -d --name ovaultd -p 127.0.0.1:8080:8080 -v ovaultd-data:/data \
  ghcr.io/kastormdz/ovaultd:0.2.3
```

The image on GHCR is the exact build the end-to-end suite verified — the release re-tags it
instead of rebuilding, so the digest is the tested one:

| Registry | Image |
| --- | --- |
| GitHub Container Registry | `ghcr.io/kastormdz/ovaultd:0.2.3` |

What changed in each version lives in [CHANGELOG.md](CHANGELOG.md), and the release notes are taken from there — never generated prose.

## Docs

- `docs/protocol.md` — wire protocol v2 (control, data plane, algorithms)
- `docs/crypto.md` — the `sync2` crypto spec and golden vectors
- `docs/server-interfaces.md` — the server contract
- `docs/deploy.md` — Docker and reverse proxy, backups
- `docs/development.md` — building, testing and the contribution rules
- `docs/runbook.md` — operating a server: backups, restore, recovery
- `docs/decisions/` — architecture decision records

## License

The server and Rust crates are licensed under AGPL-3.0 (`LICENSE`). The TypeScript packages under `packages/` are MIT, and each one ships its own `LICENSE`. The Obsidian plugin is MIT as well, in its own repository.

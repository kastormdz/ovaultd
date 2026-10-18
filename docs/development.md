# Development

Building, testing and the rules that keep the protocol frozen. Nothing here is needed to *use*
ovaultd — this is the contributor's path, kept out of the README on purpose.

## Build and test

```bash
npm install
npm run build            # TS packages + the core runtime bundle
npm test                 # TS suites (crypto vectors, wire conformance, CLI)
npm run test:browser     # durability specs against a real Chromium
cargo test --workspace   # Rust suites
cargo xtask protocol-check          # the SAME conformance fixture, both sides
bash tests/scripts/server-smoke.sh  # full server flow, live crypto + WebSocket
bash tests/scripts/scan-canary.sh   # no plaintext anywhere in server storage
bash tests/scripts/container-e2e.sh # everything: real build + smoke + persistence
(cd spikes/crypto-interop && ./run.sh --check)   # interop against frozen vectors
```

The plugin lives in its own repository ([ovaultd-plugin](https://github.com/kastormdz/ovaultd-plugin)),
where `npm run build` also runs the anti-Node gate: the bundle must not gain a single `node:*`
import, because the same `main.js` runs on mobile.

## Rules worth knowing

- The spec is frozen before code. Protocol or crypto changes land as a new ADR, never as a silent
  edit.
- Golden vectors are regenerated only through the sanctioned freeze path (`run.sh --freeze`), after
  review, and never in CI. The protocol conformance fixture is executed by both Rust and TS; that
  is the anti-drift gate.
- `scripts/scan-leaks.sh` checks the repo for accidentally committed infrastructure details and
  credentials, in the worktree and in history. Personal patterns (your hostnames, users, subnets)
  go in the gitignored `scripts/leaks.local` — see `scripts/leaks.local.example`. Wire it as a
  pre-commit hook with `git config core.hooksPath .githooks`.
- Client changes that touch sync are verified against a real server, not only with unit tests: a
  vault with a real file tree (empty files, subdirectories) and a second device are what find the
  bugs the happy path hides.

## Layout

| | |
|---|---|
| `crates/` | the server: protocol, crypto, HTTP + WebSocket surface, admin panel |
| `packages/` | the TS core (`core`, `crypto`, `protocol`), the `cli` harness and their tests |
| `tests/` | end-to-end scripts, browser specs and the panel audit |
| `docs/` | protocol and crypto specs, ADRs, deploy guide, runbook |
| `xtask/` | repo tasks (`cargo xtask protocol-check`, `cargo xtask backup-restore`) |

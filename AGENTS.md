# Via Operator Guide

This file is written for humans and automation that need to operate Via safely.

## Purpose

Via is a private mesh control plane for machines the user owns. It should make common infrastructure tasks possible without handing an agent SSH keys, Docker sockets, or raw host secrets.

Use Via for:

- listing nodes and services
- checking health
- deploying Docker images
- reading logs
- running explicit commands on named nodes
- managing mesh secrets
- updating Via binaries across the mesh

## Safety Rules

- Prefer Via commands over SSH once a node has joined the mesh.
- Treat `via exec <node> -- <cmd>` as remote shell access.
- Do not print secret values.
- Do not expose the Via daemon to the public internet.
- Let Via auto-route through the hub when a node is off-LAN. Use `--route hub` only for diagnostics.
- Do not delete services or nodes unless the user requested that action.
- Use `via ps`, `via status`, and `via logs` before making service changes when context is unclear.
- Use `via update --check` before `via update` or `via update --all`.

## Common Commands

Inspect the mesh:

```bash
via doctor
via nodes
via ps
via services
via logs --limit 100
```

Run a node command:

```bash
via exec rig -- uptime
via exec rig -- sh -lc 'docker ps'
```

Deploy a container:

```bash
via deploy nginx:latest --to rig --name web --port 18080:80
```

Move files directly:

```bash
via move ./dist/app rig:/srv/app
via move pi:/etc/pihole/custom.list rig:/backups/pihole.list
```

Configure hub routing:

```bash
export VIA_API_KEY='<cloud-api-key-for-hosted-hub>'
via hub use hosted
via hub status
via invite create --name pi
via join <token>
via start
```

For a self-hosted hub, use a full hub URL and set `VIA_HUB_ADMIN_TOKEN` locally only when that hub requires admin auth:

```bash
export VIA_HUB_ADMIN_TOKEN='<self-hosted-admin-token>'
via hub use http://127.0.0.1:47820
```

`via join` exchanges the single-use invite for a per-node hub token before writing local mesh state. Do not reuse or log invite tokens; they contain mesh bootstrap material.

Read and operate service logs:

```bash
via logs web
via restart web
via rm web
```

Manage secrets:

```bash
via secret list
via secret set API_KEY --value '<value>'
via secret delete API_KEY
```

Update Via:

```bash
via update --check
via update --node rig
via update --all
```

## Bootstrap Behavior

`via add <ssh-host>` uses SSH only to bootstrap a node. It installs the released Via binary through the public installer, copies `~/.via/mesh.key`, initializes the node, and starts the daemon.

It must not copy local Via source, compile Via remotely, or scp a locally built Via binary.

## Release Process

Normal pushes to `main` do not deploy. Pull requests into `main` run:

```text
formatting -> tests -> build
```

Releases deploy only from version tags:

```bash
git tag v0.2.0-alpha.1
git push origin v0.2.0-alpha.1
```

The release workflow runs:

```text
formatting -> tests -> build binaries -> deploy release
```

## Hub Schema

Hub schema is managed with Lux migrations in `lux/migrations/`. Keep table and column names brief. The initial hub tables are `meshes`, `nodes`, `tokens`, `sessions`, `cmds`, `events`, and `audit`.

Hub tokens are stored as hashes in `tokens`. Invite tokens use `kind=invite` and are marked `used=true` after a successful join. Long-lived node tokens use `kind=node` and are required for command posting, node discovery, and daemon WebSocket sessions.

If `VIA_HUB_ADMIN_TOKEN` is set on a self-hosted hub process, admin endpoints require a bearer token. The CLI automatically uses the local `VIA_HUB_ADMIN_TOKEN` environment variable for self-hosted `via hub use <url>` and `via invite create`.

Hosted hub provisioning is cloud-gated instead of admin-token-gated. `via hub use hosted` sends `VIA_API_KEY` (or compatibility alias `VIA_CLOUD_API_KEY`) to the cloud API at `VIA_CLOUD_API_URL` or `https://api.via.pompeiilabs.com`, receives a short-lived signed Ed25519 grant from `POST /api/hub/provision`, then forwards it to the hosted hub at `POST /v1/grants/provision`. The hosted hub verifies the grant offline with `VIA_HUB_ISSUER_PUBKEY` and provisions the mesh/node token. The Ed25519 private key lives only in the cloud API environment; never place it in hub config, CLI config, examples, or docs.

Hosted hubs may report node lifecycle events to the cloud API using `VIA_HUB_CLOUD_INGEST_URL` and `VIA_HUB_CLOUD_INGEST_TOKEN`. These async batched events power node-hours and dashboard liveness. They must not include command payloads, secret values, invite tokens, node tokens, mesh keys, API keys, or grant signing keys.

## Local Verification

Before committing code changes, run:

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

## Important Files

- `README.md`: user-facing docs.
- `CONTRIBUTING.md`: contribution workflow.
- `SECURITY.md`: vulnerability reporting policy.
- `Justfile`: local command shortcuts.
- `install.sh`: public binary installer.
- `.github/workflows/ci.yml`: PR checks.
- `.github/workflows/release.yml`: tag release deploy.
- `src/cli.rs`: command surface.
- `src/main.rs`: command handlers.
- `src/ssh.rs`: SSH bootstrap and file transfer helpers.
- `src/rpc.rs`: encrypted/signed node RPC.
- `src/hub.rs`: hub server, relay client, invite tokens, and Lux schema setup.
- `src/security.rs`: encryption/signing/key utilities.
- `src/state.rs`: embedded Lux state.
- `src/docker.rs`: Docker operations.

## Security Model

Via currently uses a shared mesh key. A node that can read that key has mesh authority. RPC payloads are encrypted and signed before direct or hub transport, and secret values are encrypted at rest. The hub requires node tokens but must still be treated as relay/auth infrastructure, not as the root cryptographic authority.

The two-tier hosted model is: CLI API key -> cloud API account check -> short-lived signed Ed25519 grant -> hub offline verification -> per-node hub token. Self-hosted/OSS mode is still admin-token-only with zero cloud dependency when no issuer public key or ingest env vars are configured. Grant replay protection depends on short TTLs plus `jti` tracking; JWKS/key rotation and command-level metering are intentionally deferred.

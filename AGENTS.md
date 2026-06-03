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
export VIA_HUB_ADMIN_TOKEN='<token-if-hosted-hub-requires-it>'
via hub use hosted
via hub status
via invite create --name pi
via join <token>
via start
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

If `VIA_HUB_ADMIN_TOKEN` is set on the hub process, admin endpoints require a bearer token. The CLI automatically uses the local `VIA_HUB_ADMIN_TOKEN` environment variable for `via hub use` and `via invite create`.

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

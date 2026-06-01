# Via

<p align="center">
  <img src="assets/via-logo-512.png" alt="Via logo" width="180">
</p>

Via is a private mesh control plane for machines you own.

It lets a laptop, desktop rig, Mac mini, Raspberry Pi, or server join one small trusted mesh so you can deploy containers, inspect services, run commands, read logs, and sync encrypted state without giving every tool SSH keys or raw Docker socket access.

Via is intentionally CLI-first. No hosted dashboard, no website, no account system.

## Why

Homelab and local infrastructure workflows usually collapse into a pile of SSH aliases, shell scripts, copied `.env` files, and one-off Docker commands.

Via is trying to make that feel like one coherent machine:

```bash
via ps
via exec rig -- uptime
via deploy nginx:latest --to rig --name web --port 18080:80
via logs web
```

The long-term direction is an agent-safe control plane: AI tools can receive narrow Via capabilities for deploy/log/status workflows without getting SSH, host secrets, or broad shell access.

## Current Status

Via is early but usable on machines you control.

Works today:

- initialize a mesh
- add nodes over SSH bootstrap
- run encrypted RPC between nodes
- deploy Docker images
- deploy with command arguments
- inspect service/container state
- read service logs
- run host commands on nodes
- store and sync encrypted secrets
- inspect Via audit/system logs
- install release binaries
- check/install Via updates across the mesh

Not ready yet:

- public internet daemon exposure
- per-node identity keys
- capability tokens
- service-scoped secret grants
- launchd/systemd daemon installation
- NAT traversal / relay mode

For now, use Via on a LAN or a VPN/overlay network like Tailscale or WireGuard.

## Install

Install the latest release:

```bash
curl -fsSL https://raw.githubusercontent.com/pompeii-labs/via/main/install.sh | bash
```

Install a specific release:

```bash
curl -fsSL https://raw.githubusercontent.com/pompeii-labs/via/main/install.sh | bash -s -- 0.1.0
```

The installer detects your OS/arch, downloads the matching GitHub release asset, verifies its SHA-256 checksum when available, and installs the binary to:

```text
~/.via/bin/via
```

It also adds `~/.via/bin` to your shell profile when it is not already on `PATH`.

For local development, symlink the debug binary somewhere on your PATH:

```bash
cargo build
ln -sf "$PWD/target/debug/via" ~/.local/bin/via
```

## Quick Start

Initialize the first node:

```bash
via init --name laptop
```

Add another machine reachable over SSH:

```bash
via add rig
```

Check mesh health:

```bash
via doctor
via nodes
via node ping rig
```

Run a command on a node through Via:

```bash
via exec rig -- sh -lc 'hostname && uptime'
```

Deploy a container:

```bash
via deploy nginx:latest --to rig --name web --port 18080:80
```

Deploy with command arguments:

```bash
via deploy alpine:latest --to rig --name worker -- sh -lc 'while true; do date; sleep 5; done'
```

Inspect services:

```bash
via ps
via services
via status web
via open web
```

Operate services:

```bash
via logs web
via stop web
via restart web
via rm web
```

Manage mesh secrets:

```bash
via secret set API_KEY --value super-secret
via secret list
via secret delete API_KEY
```

Read Via audit/system logs:

```bash
via logs
via logs --limit 100
```

Manage nodes:

```bash
via node addr rig 10.0.0.123:47819
via node rename rig laboratory
via node rm laboratory
```

Check and install Via updates:

```bash
via update --check
via update
via update --node rig
via update --all
```

`via update --all` installs the new binary on each reachable node. Restart running Via daemons after updating so they execute the new version.

## Security Model

Via currently uses a shared mesh key copied during `via add`. Treat that key as root authority for the mesh.

Implemented:

- AES-256-GCM encrypted secrets at rest.
- AES-256-GCM encrypted RPC request and response payloads.
- HMAC-signed RPC frames.
- Timestamp validation.
- Nonce replay rejection.
- Unix mesh key permissions hardened to `0600`.
- Audit events for mesh, node, service, and secret operations.
- Secret audit events do not store values or ciphertext.
- Service audit events do not store deploy command arguments.
- Node exec audit events do not store command text.

Boundaries:

- Via is not ready for raw public internet daemon exposure.
- A compromised node can currently compromise the shared mesh key.
- Node exec is intentionally powerful and should be treated like remote shell access.
- Secrets are currently injected broadly during deploy; service-scoped grants are planned.

Near-term security roadmap:

- per-node identity keys or mTLS
- node revocation
- service-scoped secret grants
- agent-scoped capability tokens
- daemon rate limiting / lockouts

## Development

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo llvm-cov --summary-only
```

Build a local release binary:

```bash
cargo build --release
via --version
```

## CI / Releases

The repo includes GitHub Actions workflows but no GitHub repository is created by this project yet.

CI runs on pushes and pull requests:

```text
.github/workflows/ci.yml
```

Release builds are tag-based. Push a version tag like `v0.1.0`:

```bash
git tag v0.1.0
git push origin v0.1.0
```

That runs:

```text
.github/workflows/release.yml
```

The release workflow builds and packages:

- `via-linux-x86_64.tar.gz`
- `via-linux-arm64.tar.gz`
- `via-macos-x86_64.tar.gz`
- `via-macos-arm64.tar.gz`

## Project Shape

Via is a Rust binary crate. It uses embedded Lux for local state and Docker for container runtime operations.

State currently uses Lux KV/documents plus a Lux table for audit events. More of the state model will move to Lux tables as the data layer stabilizes.

## License

MIT

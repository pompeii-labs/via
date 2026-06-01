# Via

<p align="center">
  <img src="assets/via-logo-512.png" alt="Via logo" width="180">
</p>

Via is a private control plane for machines you own.

It joins laptops, desktops, Raspberry Pis, Mac minis, and servers into one trusted mesh. From any node, you can inspect the mesh, run commands on another node, deploy containers, read logs, manage secrets, and update Via without handing every tool SSH keys or raw Docker access.

Via is CLI-first. There is no hosted account, dashboard, or website requirement.

## Use Cases

Run homelab services from one machine:

```bash
via deploy nginx:latest --to rig --name web --port 18080:80
via ps
via logs web
via open web
```

Manage a small hardware fleet:

```bash
via nodes
via node ping rig
via exec mac-mini -- uptime
via exec pi -- df -h
```

Deploy worker containers with command arguments:

```bash
via deploy alpine:latest --to rig --name ticker -- sh -lc 'while true; do date; sleep 5; done'
via logs ticker
```

Keep secrets inside the mesh instead of copying `.env` files around:

```bash
via secret set OPENAI_API_KEY --value sk-...
via secret list
via deploy ghcr.io/acme/worker:latest --to rig --name worker
```

Give automation a narrower control surface than SSH:

```bash
via exec rig -- docker ps
via deploy nginx:latest --to rig --name preview --port 18081:80
via logs preview
via rm preview
```

Update every reachable node:

```bash
via update --check
via update --all
```

## Concepts

**Mesh**

A mesh is the set of machines sharing Via state and a mesh key. Initialize it once with `via init`, then add nodes with `via add`.

**Node**

A node is a machine in the mesh. Each node has a slug, daemon address, and local state. Nodes can run commands against one another through Via RPC.

**Daemon**

The daemon listens on port `47819` by default. It receives encrypted RPC requests from other Via nodes and performs local operations such as Docker deploys, logs, status checks, and node exec.

**Service**

A service is a Docker container Via knows about. Via records which node owns it, the target image/path, container name, port mapping, command args, and status.

**Secret**

A secret is encrypted at rest with the mesh key and synced through mesh state. Deploys currently receive mesh secrets as environment variables.

## Network Model

Use Via on machines reachable over a private network: LAN, Tailscale, WireGuard, or a similar overlay.

Via daemons are not meant to be exposed directly to the public internet. Treat daemon reachability as administrative reachability.

SSH is used only for bootstrap in `via add`. After a node joins, day-to-day control happens over Via RPC.

## Install

Install the latest release:

```bash
curl -fsSL https://raw.githubusercontent.com/pompeii-labs/via/main/install.sh | bash
```

Install a specific release:

```bash
curl -fsSL https://raw.githubusercontent.com/pompeii-labs/via/main/install.sh | bash -s -- 0.1.0
```

The installer detects OS/architecture, downloads the matching GitHub release asset, verifies the SHA-256 checksum when available, installs the binary to:

```text
~/.via/bin/via
```

and adds `~/.via/bin` to the shell profile when needed.

For local development:

```bash
cargo build
ln -sf "$PWD/target/debug/via" ~/.local/bin/via
```

## First Mesh

Initialize the first node:

```bash
via init --name laptop
```

Add another machine reachable over SSH:

```bash
via add rig
```

`via add` does four things:

1. SSHes into the target machine.
2. Runs the release installer on the target.
3. Copies the mesh key to `~/.via/mesh.key`.
4. Initializes the node and starts the daemon.

It does not copy Via source, compile remotely, or scp a local Via binary.

Check the mesh:

```bash
via doctor
via nodes
via node ping rig
```

## Daily Operations

Run a command on a node:

```bash
via exec rig -- sh -lc 'hostname && uptime'
```

Inspect services:

```bash
via ps
via services
via status web
via open web
```

Read logs:

```bash
via logs web
via logs
via logs --limit 100
```

Operate services:

```bash
via stop web
via restart web
via rm web
```

Manage nodes:

```bash
via node addr rig 10.0.0.123:47819
via node rename rig laboratory
via node rm laboratory
```

Manage secrets:

```bash
via secret set API_KEY --value super-secret
via secret list
via secret delete API_KEY
```

Update Via:

```bash
via update --check
via update
via update --node rig
via update --all
```

Updating installs the new binary. Restart running daemons after updating so long-lived daemon processes execute the new version.

## Command Reference

| Command | Purpose |
| --- | --- |
| `via init --name laptop` | Create or refresh the local mesh node. |
| `via add rig` | Bootstrap a machine over SSH and join it to the mesh. |
| `via start` | Start the local daemon in the background. |
| `via daemon` | Run the daemon in the foreground. |
| `via doctor` | Check local state, mesh key, Docker, and node daemon reachability. |
| `via nodes` | List mesh nodes. |
| `via node ping rig` | Check one node daemon. |
| `via exec rig -- <cmd>` | Run a command on a node through Via RPC. |
| `via deploy <image> --to rig --name web` | Deploy a Docker image. |
| `via ps` | Show services with live container status. |
| `via services` | Show recorded service state. |
| `via logs web` | Read service logs. |
| `via logs` | Read Via audit/system events. |
| `via open web` | Print the local/private URL for a port-mapped service. |
| `via rm web` | Remove a service and its container. |
| `via secret set KEY --value value` | Store an encrypted mesh secret. |
| `via update --all` | Install the current/latest Via release across reachable nodes. |

## Security Model

Via currently uses a shared mesh key. That key is copied during `via add` and is root authority for the mesh.

Implemented:

- AES-256-GCM encrypted secrets at rest.
- AES-256-GCM encrypted RPC request and response payloads.
- HMAC-signed RPC frames.
- Timestamp validation.
- Nonce replay rejection.
- Unix mesh key permissions set to `0600`.
- Audit events for mesh, node, service, secret, update, and exec operations.
- Secret audit events omit values and ciphertext.
- Service audit events omit deploy command arguments.
- Node exec audit events record node/argc/locality, not command text.

Operational boundaries:

- Do not expose the daemon directly to the public internet.
- Treat `via exec` as remote shell access.
- A compromised node can read the shared mesh key from that node.
- Secrets are mesh-wide during deploy.
- Via relies on Docker for container isolation.

## Files

```text
~/.via/bin/via       installed binary
~/.via/mesh.key      mesh key
~/.via/lux/          embedded Lux state
~/.via/logs/         Via logs
~/.via/daemon.log    daemon stdout/stderr
~/.via/daemon.pid    daemon pid file
```

## Development

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test --locked --all-targets
cargo llvm-cov --summary-only
```

With `just`:

```bash
just check
```

Build a local release binary:

```bash
cargo build --locked --release
via --version
```

## CI And Releases

Pull requests into `main` run:

```text
formatting -> tests -> build
```

The workflow is:

```text
.github/workflows/ci.yml
```

Releases are tag-based:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The release workflow is:

```text
.github/workflows/release.yml
```

It runs:

```text
formatting -> tests -> build binaries -> deploy release
```

Release assets:

- `via-linux-x86_64.tar.gz`
- `via-linux-arm64.tar.gz`
- `via-macos-x86_64.tar.gz`
- `via-macos-arm64.tar.gz`

## Architecture

Via is a Rust binary crate. It embeds Lux for local state and uses Docker for container runtime operations.

Important modules:

- `AGENTS.md`: operator guide for humans and automation.
- `CONTRIBUTING.md`: contribution workflow.
- `SECURITY.md`: private vulnerability reporting policy.
- `Justfile`: local development command shortcuts.
- `src/cli.rs`: CLI command definitions.
- `src/main.rs`: command handlers.
- `src/ssh.rs`: SSH bootstrap and path deploy transfer helpers.
- `src/rpc.rs`: encrypted/signed node RPC.
- `src/security.rs`: mesh key, encryption, HMAC, nonce utilities.
- `src/state.rs`: embedded Lux state model.
- `src/docker.rs`: Docker command construction and execution.

## License

MIT

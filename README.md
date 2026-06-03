# Via

<p align="center">
  <img src="assets/via-logo-512.png" alt="Via logo" width="180">
</p>

Via is a private control plane for machines you own.

It joins laptops, desktops, Raspberry Pis, Mac minis, and servers into one trusted mesh. From any node, you can inspect the mesh, run commands on another node, deploy containers, read logs, manage secrets, and update Via without handing every tool SSH keys or raw Docker access.

Via is CLI-first. It can run entirely on a private LAN, or use a Via Hub relay so nodes behind NAT can still receive encrypted mesh commands.

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

Connect LAN machines and off-LAN servers through a hub:

```bash
via auth init
via hub use hosted
via hub status
via invite create --name jack-laptop
via start
via exec rig -- hostname
```

Once a hub is configured, normal commands automatically try direct daemon access first and fall back to the hub when needed. `--route direct` and `--route hub` still exist as diagnostic flags, but they are not part of the daily path.

Move files between named nodes:

```bash
via move ./dist/app rig:/srv/app
via move rig:/var/log/app.log ./logs/rig.log
via move pi:/etc/pihole/custom.list rig:/backups/pihole.list
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

Via has two routes:

- `direct`: TCP to the node daemon address, best for LAN, Tailscale, WireGuard, or similar overlays.
- `hub`: outbound WebSocket sessions through a Via Hub, useful for machines behind NAT.

`auto` tries direct first and falls back to hub when a hub is configured.

Via daemons are not meant to be exposed directly to the public internet. Treat daemon reachability as administrative reachability.

The hub is a relay, not a command authority. CLI requests and daemon responses are encrypted and signed with the mesh key before they reach the hub, so the hub stores and forwards opaque RPC frames.

Hub HTTP and WebSocket calls require per-node hub tokens. Invite tokens are single-use registration credentials; after `via join`, the hub returns a node token and stores only its hash.

Via supports two hub control models:

- **Self-hosted/OSS hub:** the hub has zero cloud dependency. Admin endpoints are protected by `VIA_HUB_ADMIN_TOKEN` when that env var is set on the hub process, and the CLI sends `Authorization: Bearer <token>` when the same env var is set locally.
- **Hosted Pompeii Labs hub:** the CLI authenticates to the cloud API with a Via API key, the cloud API validates the account and returns a short-lived signed Ed25519 grant, and the hub verifies that grant offline before provisioning the mesh/node token. The hub only needs the issuer public key; the Ed25519 private signing key lives only in the cloud API.

Hosted hub provisioning is intentionally a narrow trust seam. The cloud API is the account/billing gate; the hub remains a dumb relay that can verify grants locally without calling the cloud API on the hot path. Grants are versioned, audience-bound to the hub URL, mesh/account-bound, expire quickly, include a `jti`/nonce, and are rejected on replay while the hub process is running.

Hosted/cloud hubs can also report node and session usage to the cloud API. Set `VIA_HUB_CLOUD_INGEST_URL` to the cloud ingest endpoint and `VIA_HUB_CLOUD_INGEST_TOKEN` to a dedicated service credential. This is optional for self-hosted/OSS hubs; when either variable is unset, the hub emits no cloud usage events.

The cloud ingest path is asynchronous and batched. Hub events contain node lifecycle metadata (`account_id`, `mesh_id`, `node_id`, `event_type`, `timestamp`) for node-hours and dashboard liveness; command payloads remain end-to-end encrypted opaque frames and are not metered or inspected by the hub.

SSH is used only for bootstrap in `via add`. After a node joins, day-to-day control happens over Via RPC.

## Install

Install the latest release:

```bash
curl -fsSL https://raw.githubusercontent.com/pompeii-labs/via/main/install.sh | bash
```

Install a specific release:

```bash
curl -fsSL https://raw.githubusercontent.com/pompeii-labs/via/main/install.sh | bash -s -- 0.2.0-alpha.1
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

## Hub Mode

Run a self-hosted hub:

```bash
via hub migrate --lux-dir /var/lib/via-hub/lux
VIA_HUB_ADMIN_TOKEN='<long-random-token>' via hub start --bind 127.0.0.1:47820 --lux-dir /var/lib/via-hub/lux
```

For local development you can combine both:

```bash
via hub start --bind 127.0.0.1:47820
```

Point a mesh at a hub:

```bash
export VIA_HUB_ADMIN_TOKEN='<long-random-token>'
via hub use http://127.0.0.1:47820
via hub status
via start
```

`via hub use` registers the local node with the hub and stores a node token in `~/.via/hub.json`.
Pass a full URL for a self-hosted hub. Use `VIA_HUB_ADMIN_TOKEN` locally when the self-hosted hub requires admin auth.

Use the hosted Pompeii Labs hub:

```bash
via auth init
via hub use hosted
via hub status
```

`via auth init` prompts for the API key from the dashboard and stores it in `~/.via/auth.json`. `via hub use hosted` defaults to `https://api.via.pompeiilabs.com` and `https://hub.via.pompeiilabs.com`. Use `VIA_API_KEY` as a CI or non-interactive override; `VIA_CLOUD_API_KEY` remains a compatibility alias. Override service URLs with `VIA_CLOUD_API_URL` and `VIA_HOSTED_HUB_URL` for staging.

Create an invite for another node:

```bash
via invite create --name pi
```

Join on the other machine:

```bash
via join via1...
via start
```

The `via1...` invite is single-use. `via join` exchanges it with the hub for a node token before writing local mesh state.

Normal commands automatically use the hub as fallback when direct daemon access fails:

```bash
via node ping pi
via exec pi -- uptime
via deploy nginx:latest --to pi --name web --port 18080:80
```

Use hidden diagnostic flags such as `--route hub` only when testing a specific transport.

Hub schema lives in Lux migrations under `lux/migrations/`. Table and column names intentionally stay short: `meshes`, `nodes`, `tokens`, `sessions`, `cmds`, `events`, and `audit`.

`via nodes` includes nodes discovered through the hub when hub routing is configured.

## Hosted Cloud Contract

The Via hub and Via Cloud stay separate services. The cloud API owns account validation, API keys, grant signing, usage ingest, and dashboard state. The Rust hub owns encrypted relay, node tokens, invite tokens, and offline grant verification.

### CLI to cloud to hub

For the hosted hub, the CLI sends `POST /api/hub/provision` to the cloud API with `Authorization: Bearer <via-api-key>` and this JSON body:

```json
{
  "hub_url": "https://hub.via.pompeiilabs.com",
  "mesh_id": "mesh_...",
  "mesh_name": "default",
  "node_id": "node_...",
  "node_slug": "laptop"
}
```

The cloud API returns `grant` or `signed_grant`. The CLI forwards that value to the hub with `POST /v1/grants/provision`:

```json
{
  "grant": "viahub1.<payload>.<signature>",
  "node_id": "node_...",
  "node_slug": "laptop"
}
```

The grant payload is base64url JSON signed with Ed25519 and currently includes:

```json
{
  "v": 1,
  "aud": "https://hub.via.pompeiilabs.com",
  "exp": 1760000000,
  "jti": "unique-grant-id",
  "account_id": "acct_...",
  "mesh_id": "mesh_...",
  "mesh_name": "default"
}
```

The hub verifies the Ed25519 signature with `VIA_HUB_ISSUER_PUBKEY`, checks version, expiry, audience, account/mesh presence, and in-process `jti` replay before minting the per-node hub token. If `VIA_HUB_ISSUER_PUBKEY` is unset, `/v1/grants/provision` is disabled and the self-hosted admin-token flow remains unchanged.

### Hub to cloud ingest

When configured, the hub batches node lifecycle events to the cloud ingest URL with `Authorization: Bearer <service-token>`:

```json
{
  "events": [
    {
      "account_id": "acct_...",
      "mesh_id": "mesh_...",
      "node_id": "node_...",
      "event_type": "connect",
      "timestamp": 1760000000
    }
  ]
}
```

The cloud API should treat this stream as the source of truth for hosted node liveness and make ingest idempotent. The dashboard reads the cloud-side `nodes`/`node_status` state derived from these events.

### Environment variables

| Variable | Service | Purpose |
| --- | --- | --- |
| `VIA_API_KEY` | CLI | CI/non-interactive override for the API key stored by `via auth init`. |
| `VIA_CLOUD_API_KEY` | CLI | Compatibility alias for hosted hub provisioning. Prefer `VIA_API_KEY`. |
| `VIA_CLOUD_API_URL` | CLI | Override the cloud API base URL; defaults to `https://api.via.pompeiilabs.com`. |
| `VIA_HOSTED_HUB_URL` | CLI | Override the hosted hub URL; defaults to `https://hub.via.pompeiilabs.com`. |
| `VIA_HUB_ADMIN_TOKEN` | Hub/CLI | Self-hosted admin bearer token for mesh creation, invite creation, and direct node registration. Not used for hosted cloud account gating. |
| `VIA_HUB_ISSUER_PUBKEY` | Hub | Base64 Ed25519 public key used to verify cloud-issued grants offline. Leave unset for pure self-hosted hubs. |
| `VIA_HUB_URL` | Hub | Canonical public hub URL used for grant audience checks, especially behind a proxy. |
| `VIA_HUB_CLOUD_INGEST_URL` | Hub | Cloud API endpoint that receives batched usage/lifecycle events. |
| `VIA_HUB_CLOUD_INGEST_TOKEN` | Hub | Dedicated service bearer token for cloud ingest. Do not reuse API keys or admin tokens. |

The Ed25519 grant signing private key is a cloud API secret only. Do not put it in hub config, CLI config, repo docs, or self-hosted examples.

### Deferred / follow-ups

- JWKS/key rotation is deliberately deferred; rotate the static Ed25519 issuer key manually until JWKS is added.
- Command-level metering is deliberately deferred because hub RPC frames are end-to-end encrypted and opaque.
- Grant replay tracking is in-process and bounded by grant expiry; keep hosted grants short-lived.
- Self-hosted hubs remain fully functional with only admin-token auth and no cloud env vars.

## Daily Operations

Run a command on a node:

```bash
via exec rig -- sh -lc 'hostname && uptime'
```

Copy files directly between nodes:

```bash
via move ./dist/app rig:/srv/app
via move pi:/etc/pihole/custom.list rig:/backups/pihole.list
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
| `via auth init` | Save a dashboard API key in `~/.via/auth.json`. |
| `via hub use hosted` | Configure this mesh to use the hosted Via hub. |
| `via hub status` | Show the active hub, auth state, and connected nodes. |
| `via hub list` | Show the one active hub configured for this mesh. |
| `via hub drop` | Disconnect this node from the active hub. |
| `via hub start` | Run a self-hosted hub and apply Lux migrations. |
| `via invite create --name pi` | Create a join token for another node. |
| `via join <token>` | Join a mesh from an invite token. |
| `via doctor` | Check local state, mesh key, Docker, and node daemon reachability. |
| `via nodes` | List mesh nodes. |
| `via node ping rig` | Check one node using automatic routing. |
| `via exec rig -- <cmd>` | Run a command on a node through Via RPC. |
| `via move ./file rig:/tmp/file` | Copy files directly between local paths and named nodes. |
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
- Hub relays store opaque encrypted RPC frames, not plaintext commands.
- Hub invite tokens are single-use and stored hashed.
- Hub node tokens are required for command posting, node discovery, and daemon WebSocket sessions.
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
~/.via/hub.json      optional hub URL/token config
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
git tag v0.2.0-alpha.1
git push origin v0.2.0-alpha.1
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
- `src/hub.rs`: Hub server, Lux migrations, invite tokens, and hub route client.
- `src/security.rs`: mesh key, encryption, HMAC, nonce utilities.
- `src/state.rs`: embedded Lux state model.
- `src/docker.rs`: Docker command construction and execution.

## License

MIT

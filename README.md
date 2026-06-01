# Via

Via is a private control plane for machines you own. It lets your laptop, rig, Mac mini, Raspberry Pi, and other trusted devices join a mesh so you can deploy containers, inspect services, read logs, and sync encrypted mesh state without handing every tool raw SSH access.

Via is early. The current implementation is designed for local/homelab testing, with security primitives in place and a narrow CLI-first workflow.

## Core Commands

```bash
via init --name laptop
via add rig
via nodes
via doctor
via ps
```

Deploy an image:

```bash
via deploy nginx:latest --to rig --name web --port 18080:80
```

Deploy with command arguments:

```bash
via deploy alpine:latest --to rig --name worker -- sh -lc 'while true; do date; sleep 5; done'
```

Operate services:

```bash
via logs web
via exec web -- printenv
via stop web
via restart web
via rm web
via open web
```

Manage secrets:

```bash
via secret set API_KEY --value super-secret
via secret list
via secret delete API_KEY
```

Inspect Via audit/system logs:

```bash
via logs
via logs --limit 100
```

Manage nodes:

```bash
via node ping rig
via node addr rig 10.0.0.123:47819
via node rename rig laboratory
via node rm laboratory
```

## Security Model

Via currently uses a shared mesh key copied during `via add`.

Implemented:

- AES-256-GCM encrypted secrets at rest.
- AES-256-GCM encrypted RPC request and response payloads.
- HMAC-signed RPC frames.
- Timestamp checks and nonce replay rejection.
- Owner-only mesh key file permissions on Unix.
- Audit events for mesh, node, service, and secret operations.
- Secret audit events are scrubbed to avoid logging secret values or ciphertext.

Important next steps:

- Per-node identity keys or mTLS, so one compromised node does not imply full mesh compromise.
- Agent-scoped capability tokens.
- Service-scoped secret grants.
- A real daemon installer for launchd/systemd.

## Install

From a checkout:

```bash
./install.sh
```

The installer builds Via with Cargo and installs it to `~/.via/bin/via` by default.

After public releases exist, the same script can install a prebuilt binary:

```bash
VIA_VERSION=0.1.0 ./install.sh
```

## Development

```bash
cargo fmt
cargo test
cargo llvm-cov --summary-only
```

Build release artifacts:

```bash
scripts/build-release.sh
scripts/package-release.sh
```

## Status

Via is not ready for untrusted networks yet. It is useful for controlled machines you own, especially homelab and local-network workflows. Treat the current shared mesh key like root authority for the mesh.

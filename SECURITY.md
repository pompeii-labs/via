# Security Policy

## Reporting A Vulnerability

Please report Via security issues privately. Do not open a public GitHub issue for vulnerabilities.

Email **hello@pompeiilabs.com** with:

- a description of the issue
- steps to reproduce
- affected versions
- expected impact
- any relevant logs or proof of concept

## Scope

This policy covers the Via CLI, daemon, hub, installer, release artifacts, and mesh RPC protocol.

## What Qualifies

- mesh authentication or authorization bypasses
- RPC payload disclosure or tampering
- hub relay plaintext access to command or response bodies
- secret value disclosure
- command injection in installer, bootstrap, deploy, update, or exec paths
- unsafe default daemon exposure
- state corruption that impacts mesh integrity

## Current Security Model

Via currently uses a shared mesh key. A node with access to that key has mesh authority. RPC payloads are encrypted and signed before direct or hub transport, and secrets are encrypted at rest. The hub should see opaque encrypted frames, not plaintext command bodies.

Hub invite tokens are single-use and stored hashed. Successful joins exchange the invite for a per-node hub token, which is required for command posting, node discovery, and daemon WebSocket sessions.

Hosted hubs should run with `VIA_HUB_ADMIN_TOKEN` set. This protects mesh creation, invite creation, and direct node registration endpoints with bearer-token authentication.

## Disclosure

We will coordinate fixes and disclosure with the reporter. Once a fix is available, we will publish a patched release and note the affected versions.

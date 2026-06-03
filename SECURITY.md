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

Via supports two hub trust models:

- **Self-hosted/OSS hubs** have no cloud dependency. If `VIA_HUB_ADMIN_TOKEN` is set on the hub, admin endpoints require bearer-token authentication for mesh creation, invite creation, and direct node registration.
- **Hosted Pompeii Labs hubs** gate provisioning through Via Cloud. The CLI sends a Via API key to the cloud API, the cloud API validates the account and signs a short-lived Ed25519 grant, and the hub verifies that grant offline before issuing a per-node hub token.

Hosted grant verification is intentionally offline. The hub is configured with only `VIA_HUB_ISSUER_PUBKEY`, a base64 Ed25519 public key. The Ed25519 private signing key must live only in the cloud API environment and must never be copied into hub config, CLI config, source docs, or self-hosted examples. If the issuer public key is unset, grant provisioning is disabled and the admin-token-only self-hosted flow remains unchanged.

Hosted grants are versioned, audience-bound to the hub URL, account/mesh-bound, short-lived, and include a `jti`/nonce. The hub rejects expired grants, audience mismatches, missing account or mesh claims, invalid signatures, and in-process `jti` replays.

Hosted hubs can optionally send batched node lifecycle events to Via Cloud with `VIA_HUB_CLOUD_INGEST_URL` and `VIA_HUB_CLOUD_INGEST_TOKEN`. The ingest token is a dedicated service credential for cloud usage/dashboard ingest only; it is not a mesh key, API key, admin token, or grant signing key. Usage events contain node metadata for liveness and node-hours, not plaintext command payloads.

Deferred security work: JWKS/key rotation is not implemented yet; static Ed25519 issuer rotation is manual. Command-level metering is intentionally omitted because hub RPC frames are end-to-end encrypted and opaque.

## Disclosure

We will coordinate fixes and disclosure with the reporter. Once a fix is available, we will publish a patched release and note the affected versions.

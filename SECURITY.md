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

This policy covers the Via CLI, daemon, installer, release artifacts, and mesh RPC protocol.

## What Qualifies

- mesh authentication or authorization bypasses
- RPC payload disclosure or tampering
- secret value disclosure
- command injection in installer, bootstrap, deploy, update, or exec paths
- unsafe default daemon exposure
- state corruption that impacts mesh integrity

## Current Security Model

Via currently uses a shared mesh key. A node with access to that key has mesh authority. RPC payloads are encrypted and signed, and secrets are encrypted at rest, but Via should only be used on trusted machines over private networks.

## Disclosure

We will coordinate fixes and disclosure with the reporter. Once a fix is available, we will publish a patched release and note the affected versions.

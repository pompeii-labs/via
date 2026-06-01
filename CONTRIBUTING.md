# Contributing To Via

Via uses focused pull requests and conventional commits.

## Setup

```bash
git clone https://github.com/pompeii-labs/via.git
cd via
cargo build
```

Via depends on the public `lux-db/lux` release pinned in `Cargo.toml`.

## Checks

Before opening a pull request, run:

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release
```

Or, with `just`:

```bash
just check
```

## Pull Requests

- Keep changes focused.
- Use conventional commits: `feat:`, `fix:`, `docs:`, `test:`, `refactor:`, `ci:`, `chore:`.
- Add or update tests for command behavior, security behavior, and release/install behavior.
- Keep README and `AGENTS.md` accurate when command behavior changes.

## Release Changes

Do not deploy from ordinary pushes to `main`. Releases are tag-based:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The release workflow runs formatting, tests, binary builds, and GitHub release publishing in sequence.

## License

By contributing, you agree that your contributions are licensed under the MIT License.

# Contributing

Thanks for helping improve AI Terminal Coach.

Start with the [architecture map](docs/ARCHITECTURE.md) to find the owning crate and review the
[IPC contract](docs/PROTOCOL.md) before changing any cross-process message. These guides also define
the provider, terminal-ownership, retention, and compatibility boundaries a contribution must keep.

## Development setup

The project targets macOS 13+, Zsh 5.8+, and Rust 1.88+. Fork the repository,
create a focused branch, and keep changes scoped to one concern.

Before submitting a change, run:

```zsh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
zsh scripts/test-zsh-integration.zsh
zsh scripts/test-onboarding-e2e.zsh
zsh scripts/benchmark-zsh-hooks.zsh
cargo build --release --workspace --locked
zsh scripts/benchmark-product.zsh
```

AI provider tests must use local mocks. Never commit API keys, credentials,
terminal history, private repository content, local configuration, logs, or
generated release archives.

Run `zsh scripts/check-no-secrets.sh` and `zsh scripts/check-actions-pinned.sh`
before pushing. The same credential and immutable-action regression checks run
in CI.

Performance budgets and measurement details are documented in
[docs/PERFORMANCE.md](docs/PERFORMANCE.md). Include before/after numbers when changing synchronous
Zsh hooks or adding a release dependency.

## Pull requests

Describe the user-visible behavior, the tests you ran, and any privacy or safety
impact. State what is retained, what can reach a provider, how cancellation and
provider failure behave, and which connection owns any UI mutation. Keep AI
suggestions non-executing: a suggestion may only update the visible ZLE buffer,
and the user must always press Enter.

By contributing, you agree that your contribution is licensed under the
project's MIT License.

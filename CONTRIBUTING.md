# Contributing

Thanks for helping improve AI Terminal Coach.

## Development setup

The project targets macOS 13+, Zsh 5.8+, and Rust 1.85+. Fork the repository,
create a focused branch, and keep changes scoped to one concern.

Before submitting a change, run:

```zsh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
zsh scripts/test-zsh-integration.zsh
```

AI provider tests must use local mocks. Never commit API keys, credentials,
terminal history, private repository content, local configuration, logs, or
generated release archives.

## Pull requests

Describe the user-visible behavior, the tests you ran, and any privacy or safety
impact. Keep AI suggestions non-executing: a suggestion may only update the
visible ZLE buffer, and the user must always press Enter.

By contributing, you agree that your contribution is licensed under the
project's MIT License.

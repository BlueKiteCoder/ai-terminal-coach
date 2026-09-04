## User-visible outcome

Describe the terminal workflow that changed and why it is useful.

## Verification

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] `zsh scripts/test-zsh-integration.zsh`

## Safety and privacy

- [ ] AI suggestions remain visible and non-executing; the user must press Enter.
- [ ] New retained or provider-bound data is documented, bounded, and redacted.
- [ ] Tests and examples contain no real credentials or private terminal/repository content.

## Compatibility

List the macOS version, architecture, terminal application, and Zsh setup used for manual testing.

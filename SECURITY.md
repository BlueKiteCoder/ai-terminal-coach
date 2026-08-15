# Security

AI Terminal Coach never executes an AI suggestion. Shell completions and TUI
recommendations only replace or insert text in the current ZLE buffer; the user
must press Enter.

Report vulnerabilities privately through the repository security advisory
feature. Do not include API keys, terminal history, private repository content,
or full diagnostic logs in a report.

The recommended credential storage is macOS Keychain via
`aicoach config set-key`. Plaintext credentials must not be committed to this
repository or written to `config.toml`.

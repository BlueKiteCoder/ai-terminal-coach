# Changelog

All notable user-visible changes are recorded here. This project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-09-04

### Added

- Local-first macOS/Zsh failure analysis with bounded, redacted terminal context.
- Structured OpenAI-compatible completion, analysis, and streaming chat with cancellation.
- Risk Lens preflight impact summaries and terminal-safe destructive-command warnings.
- Source Cards backed by bounded local manual and allowlisted Apple Git help excerpts.
- Explainable token-level Command Patches that scan the complete resulting buffer.
- Privacy-scrubbed Session Capsules for sharing a reproducible troubleshooting timeline.
- English and Simplified Chinese output, physical Option-key onboarding, and live key reload.
- Native Ratatui Coach window plus an optional macOS global hotkey helper.
- Reproducible Apple Silicon and Intel archive automation with checksums, manifests, signature and
  minimum-version verification, GitHub attestations, and fail-closed Apple notarization gates.
- Upgrade-safe Homebrew LaunchAgent paths plus an offline-build-compatible HEAD formula.

### Security

- No provider URL, model, or API key is bundled; the default provider is disabled.
- CI rejects API-key-shaped strings and removed private-provider markers in tracked source.
- Suggestions remain visible and non-executing, and AI Terminal Coach never intercepts Enter.
- Provider-bound content is redacted by default; Session Capsules are always redacted.

[Unreleased]: https://github.com/BlueKiteCoder/ai-terminal-coach/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/BlueKiteCoder/ai-terminal-coach/releases/tag/v0.1.0

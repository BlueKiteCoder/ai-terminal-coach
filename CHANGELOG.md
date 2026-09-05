# Changelog

All notable user-visible changes are recorded here. This project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Coach suggestions now carry an always-visible local Risk Lens badge. Insert-only and copy-only
  receipts state the destination, classification, coverage limits, and that nothing was executed;
  the daemon independently reclassifies terminal handoffs so a client cannot spoof a lower risk.
- Failure Fingerprints locally recognize recurring failures and surface the redacted command that
  next succeeded last time, with bounded retention plus `aicoach memory status|list|clear` controls.
- Environment Drift Lens compares a failed command with the latest success in the same session and
  surfaces only changed cwd, Python/Conda activation, and bounded Git metadata without AI access.
- Session Checkpoints name a bounded troubleshooting interval, focus its Capsule timeline, and add
  a final resolution entered interactively outside normal Shell history.
- Local Data Controls inventory every persistent, Keychain, runtime, and daemon-memory category
  without content, then clear one session, history, fingerprints, logs, or all transient data.

### Security

- GitHub Actions are pinned to immutable commits, run on current Node 24 action releases,
  and receive weekly Dependabot update pull requests.
- Failure memory never persists the failed command, diagnostic output, cwd, or session ID; it is
  owner-only, force-redacted independently of provider settings, and never added to AI prompts.
- Environment Drift baselines remain in daemon memory, read no repository file contents, omit
  incomplete Git probes, and never add their comparison report to provider prompts.
- Checkpoint names and resolutions are terminal-safe, length-bounded, daemon-memory-only, removed
  from all provider prompts, and force-redacted when a Capsule is exported.
- Data inventory never prints stored content or reads the Keychain secret; session clearing
  cancels active AI work, removes pending failure links, and cannot delete config or credentials.

### Fixed

- Source Card process integration tests tolerate contended CI scheduling without changing the
  product's 800ms local-documentation timeout.

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

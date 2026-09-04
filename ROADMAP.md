# AI Terminal Coach Roadmap

AI Terminal Coach is intentionally focused on macOS and Zsh. Its product promise is:

> Invisible while the terminal works; immediately useful when it does not.

The project does not chase feature count. A feature belongs here only when it is useful in a
real terminal workflow, can be explained in one sentence, preserves user control, and has an
honest privacy boundary.

## Product guardrails

- Never execute an AI suggestion or intercept Enter.
- Keep the shell responsive when the daemon or provider is unavailable.
- Prefer local analysis before network requests.
- Collect bounded context, redact before provider upload, and never log prompt contents.
- Make every high-impact action visible, reversible where possible, and initiated by the user.
- Keep the core useful without an API key.

## Milestone 1 — A product people can safely recommend

- [x] Structured, cancellable AI completion and streaming chat.
- [x] Local failure analysis and pre-execution safety warnings.
- [x] English and Simplified Chinese UI/output.
- [x] Privacy-first provider boundary and macOS Keychain integration.
- [x] **Session Capsule:** locally export a redacted, terminal-safe Markdown incident report.
- [ ] Signed and notarized Apple Silicon and Intel release artifacts.
- [ ] Working Homebrew tap with an upgrade-safe install flow.
- [ ] A two-minute interactive onboarding and shortcut verifier.
- [ ] Demo assets showing the complete failure → diagnosis → fix workflow.

Exit gate: a new user can install, configure, understand the safety model, experience the core
workflow, and uninstall on a clean supported Mac without reading source code.

## Milestone 2 — Explainable command collaboration

- [x] **Command Patch:** preview an AI completion as a token-level diff with a short reason for
  every materially changed flag, path, and subcommand, then scan the composed final buffer.
- [x] **Risk Lens:** explicitly inspect the current ZLE buffer and show what the command can
  modify, what privileges it needs, and which parts are irreversible—without blocking Enter or
  calling an AI provider; unfamiliar commands are honestly marked unrated.
- [ ] **Source Cards:** attach local `man`/`--help` evidence to supported command explanations so
  users can distinguish documented behavior from model inference.
- [ ] One-keystroke “insert only” and “copy only” actions with a visible safety classification.

Exit gate: users can understand why a suggestion changed, where its claims came from, and what
could happen before they choose to run it.

## Milestone 3 — Local memory that earns daily use

- [ ] **Failure Fingerprints:** detect recurring failures locally and surface the fix that worked
  the last time, without uploading a long-lived personal command history.
- [ ] **Environment Drift Lens:** compare safe environment/Git metadata between the last success
  and current failure to expose changed branches, virtual environments, directories, or tools.
- [ ] **Session Checkpoints:** let users name a bounded troubleshooting session and append the
  final resolution to its Capsule.
- [ ] Local retention controls, per-session deletion, and a complete data inventory command.

Exit gate: the Coach gets more useful on a developer's own Mac while its retained data remains
inspectable, bounded, and removable.

## Milestone 4 — Open-source growth infrastructure

- [x] Stable issue templates for bugs, terminal compatibility, feature proposals, and security.
- [ ] A public compatibility matrix covering Terminal.app, iTerm2, Warp, WezTerm, kitty, and
  Alacritty on supported macOS releases.
- [ ] Contributor-friendly module boundaries and protocol documentation.
- [ ] Reproducible release automation, checksums, changelog, and upgrade notes.
- [ ] Small benchmark dashboard for shell hook latency, startup time, and binary size.

Exit gate: contributors can reproduce failures, add an analyzer rule or terminal adapter, and
verify their change without access to a private AI provider.

## How progress is measured

Ten thousand GitHub stars is an aspiration, not a correctness test and not something code alone
can guarantee. Leading indicators are more actionable:

- clean-install success rate;
- time from installation to the first useful result;
- weekly returning users;
- Session Capsules created and voluntarily shared;
- issue-to-fix time and outside contributors;
- shell-hook latency regressions and crash-free sessions.

Roadmap items may change when user evidence contradicts them, but the product guardrails do not.

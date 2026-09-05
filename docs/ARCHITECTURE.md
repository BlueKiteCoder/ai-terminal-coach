# Architecture

This document is the map for changing AI Terminal Coach without weakening its product, privacy,
or safety boundaries. It describes `main` and protocol version 2. The Rust types remain the source
of truth when this guide and code disagree.

## Non-negotiable invariants

Every change must preserve these properties:

1. The synchronous Zsh hooks do bounded local work and never wait for the daemon or an AI provider.
2. A provider failure cannot break normal shell input or command execution.
3. AI output never presses Enter. It may only become visible text or a visible ZLE buffer proposal.
4. The daemon rechecks command safety before a proposal reaches ZLE; a client-supplied label is not
   authoritative.
5. Retained context is session-isolated, bounded, inspectable, and removable.
6. Provider-bound command, output, path, Git, and chat content passes through the configured privacy
   redactor. Long-lived local-memory metadata is not added to provider prompts.
7. Logs record operation metadata and safe error kinds, never request or response bodies.

## Process topology

```text
Zsh hooks / ZLE widgets             aicoach CLI             aicoach-ui
          |                              |                       |
          +------------- Unix domain socket --------------------+
                                         |
                                     aicoachd
                         +---------------+----------------+
                         |               |                |
                    local engines   bounded sessions   AI provider
                    (no network)      and routing      (redacted,
                                                        opt-in)
```

The socket normally lives at `~/.aicoach/run/aicoach.sock`. The containing product directories are
owner-only. Zsh uses a compact tab/percent-encoded protocol to keep hook overhead low. Rust clients
use newline-delimited JSON. Both transports decode into the same typed `aicoach-ipc` messages.

The optional Carbon helper only toggles the managed Coach window. It does not capture commands,
own sessions, call a provider, or execute suggestions.

## Dependency direction

Arrows mean “depends on.” Keep dependencies pointed toward reusable policy and data types:

```text
aicoach-ai      -> aicoach-core
aicoach-ipc     -> aicoach-core
aicoach-daemon  -> aicoach-ai + aicoach-ipc + aicoach-core
aicoach-cli     -> aicoach-ipc + aicoach-core
aicoach-tui     -> aicoach-ipc + aicoach-core
```

`aicoach-core` must not depend on a terminal UI, daemon runtime, transport, or provider client.
`aicoach-ipc` owns the cross-process contract, not daemon policy. The daemon is the only component
that combines local policy, session state, routing, and provider access.

## Ownership by module

| Area | Owns | Does not own |
|---|---|---|
| `aicoach-core` | Config, privacy, local analysis, safety, Risk Lens, Command Patch, Source Cards, Git metadata, Failure Fingerprints | Sockets, UI state, provider HTTP |
| `aicoach-ai` | Provider trait, OpenAI-compatible HTTP/JSON/SSE, cancellation, retry, timeouts, credential-safe errors | Session retention, ZLE mutation, local safety policy |
| `aicoach-ipc` | Typed requests/responses/events, identifiers, frame limits, JSON and Zsh codecs | Authorization policy, request execution |
| `aicoach-daemon` | Session lifecycle, request routing, cancellation, event delivery, local-first orchestration, provider boundary | Installation, terminal key capture |
| `aicoach-cli` | Install/uninstall, LaunchAgents, Keychain setup, configuration, doctor, Capsule, checkpoints, data controls | Interactive chat rendering, provider calls |
| `aicoach-tui` | Ratatui state, input, streaming display, scrolling, safe copy/insert requests, bounded disk chat history | Shell ownership, direct ZLE mutation |
| `shell/aicoach.zsh` | `preexec`/`precmd`, allowlisted environment snapshot, ZLE buffer ownership, physical shortcuts | AI HTTP, long-lived policy decisions |
| `macos/` and `scripts/` | Global hotkey helper and Terminal.app/iTerm2 window coordination | Command collection or execution |

## Four important flows

### Command failure

1. `preexec` sends a command-start frame and returns immediately.
2. `precmd` sends exit status, duration, cwd, and allowlisted environment metadata.
3. The daemon bounds and sanitizes the session record.
4. Local analysis, failure-memory association, and Environment Drift run without a provider.
5. Only a failure that still needs help can create a redacted provider analysis request.
6. Hints are routed to the owning shell and subscribed Coach windows.

### Completion and insertion

1. A ZLE widget snapshots `BUFFER` and the character cursor.
2. A newer request cancels the older completion for that session.
3. Provider output is parsed into a typed completion; malformed output is rejected.
4. Command Patch and Risk Lens evaluate the composed final buffer locally.
5. The daemon sends the proposal only to the shell connection that owns the session and supports
   buffer insertion.
6. ZLE applies it only if the user's buffer still matches the original snapshot. Enter remains the
   user's action.

### Streaming chat

Chat deltas and completion/failure events return to the connection that created the request. This
prevents a TUI stream from appearing as shell output. Session notifications use a separate route:
the shell owner plus clients that subscribed by requesting context or chat for that session.

### Local data clearing

The daemon clears and cancels in-memory state first, then emits `data_cleared` to open Coach windows
before acknowledging the CLI. The CLI handles disk files with typed scopes. A late shell FINISH
frame for an erased in-flight command is consumed without recreating the deleted context. Config,
support files, the shell backup, and Keychain credentials are outside every data-clear scope.

## State and trust boundaries

The most important boundary is not “local versus network”; it is where untrusted data is accepted:

- Shell and JSON clients are untrusted. The daemon reapplies environment allowlists, terminal-text
  sanitization, length budgets, session ownership rules, and safety classification.
- Provider output is untrusted. Structured fields are validated, terminal controls are removed,
  cursors are clamped by character count, and the final command is rescanned locally.
- Files are untrusted. Malformed config and memory files are reported rather than overwritten;
  destructive file operations use known paths and narrow filename allowlists.
- Terminal display APIs are best effort. Screen-tail capture is opt-in, bounded, sanitized, and must
  never become a correctness requirement.

See [PROTOCOL.md](PROTOCOL.md) for message routing and compatibility rules. Use
`aicoach data status --json` to inspect the current retention categories and limits without printing
their contents.

## Extension recipes

### Add a local analyzer or safety rule

1. Put pure recognition and result types in `aicoach-core`.
2. Test positive, negative, quoted/commented, compound-command, Unicode, and oversized-input cases.
3. Localize only presentation in the daemon; keep the structured result language-neutral.
4. Prove the path makes zero provider calls with an integration test.
5. If the rule can propose a command, test the complete composed buffer through Safety Engine.

### Add an IPC operation

1. Add typed parameters/results/events in `aicoach-ipc/src/protocol.rs`.
2. Prefer optional fields with `#[serde(default)]` for additive compatibility.
3. Add JSON round-trip tests and Zsh codec coverage only if a shell widget needs the operation.
4. Implement daemon authorization, session validation, bounds, cancellation, and routing.
5. Add an IPC integration test that exercises a real Unix socket.
6. Update [PROTOCOL.md](PROTOCOL.md). Bump `PROTOCOL_VERSION` for a breaking wire change.

### Add retained data

1. Define a strict size/count/time limit before writing the store.
2. Use owner-only directories, an owner-only atomic replacement, and a maximum file size before
   parsing.
3. Specify whether the content can enter a provider request.
4. Add the category and exact deletion behavior to `aicoach data`.
5. Test malformed data, forward-compatible fields, symlinks where relevant, and interrupted writes.

### Add a terminal adapter

Keep command/session capture in portable Zsh. A terminal-specific adapter should only improve window
placement, focus, or optional screen-tail capture. It must degrade to normal Zsh behavior when its
native API is absent. Add the terminal and exact capability level to the compatibility matrix only
after a real supported-macOS test.

## Definition of done

Run the complete local gates documented in [CONTRIBUTING.md](../CONTRIBUTING.md). A pull request is
not complete until it explains user-visible behavior, cancellation and failure behavior, retained
data, provider exposure, terminal ownership, and how the feature behaves without an API key.

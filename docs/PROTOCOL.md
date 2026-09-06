# IPC Protocol

AI Terminal Coach uses a versioned local protocol between Zsh, the CLI, the TUI, and `aicoachd`.
This guide explains the contract contributors must preserve. The authoritative types are in
`crates/aicoach-ipc/src/protocol.rs`; codecs and frame handling live beside them.

Current protocol version: **4**

## Transport

- Endpoint: Unix domain socket, normally `~/.aicoach/run/aicoach.sock`.
- Default Rust-client frame limit and daemon inbound-line limit: 4 MiB. Domain payloads have lower
  content-specific budgets.
- JSON clients: one UTF-8 JSON object per line (NDJSON).
- Zsh: `ZSH` plus tab-separated fields with percent encoding for tabs, newlines, `%`, and other
  delimiter-sensitive content.
- A connection chooses its wire format with its first frame and cannot switch formats later.
- Empty, malformed, oversized, or mixed-protocol connections are closed without executing work.

The Zsh codec is deliberately an internal performance adapter. Third-party clients should use the
typed NDJSON protocol.

## Handshake and identities

JSON clients should send `hello` first with:

- `protocol_version`;
- a diagnostic `client_name` and `client_version`;
- `client_kind`: `shell`, `tui`, `cli`, or `test`;
- capabilities for pushed events, streaming, buffer insertion, and the shell line protocol.

Capabilities describe client behavior. Buffer insertion is an enforced routing gate; the remaining
flags are currently negotiation metadata reserved for compatible routing refinements.

The daemon rejects an incompatible version and requires a completed JSON handshake before session
registration. Only a `shell` client can own a live shell session. A `tui` can create a detached
session or subscribe to an existing one, but it cannot claim ZLE buffer ownership. `cli` and `test`
clients cannot register as shell owners.

Zsh line-protocol connections receive an implicit shell identity and capabilities because the
codec itself is bundled with the same installation.

## Envelope model

Every identifier is a UUID. `request_id` correlates one response and any request-scoped events.
`session_id` isolates terminal context and is required for session operations.

An illustrative ping request is:

```json
{"type":"request","request_id":"00000000-0000-4000-8000-000000000001","method":"ping"}
```

Requests have a flattened `method` and optional `params`. Responses repeat `request_id`, optionally
repeat `session_id`, and contain either a typed success result or a stable protocol error. Events
have their own `event_id`, a `session_id`, an optional originating `request_id`, and a typed body.

Never parse human-readable `message` text to make a control decision. Use method, result, event,
error code, and structured fields.

## Request catalog

| Method | Session | Purpose | Important boundary |
|---|---:|---|---|
| `hello` | no | Negotiate version, kind, and capabilities | Must precede JSON registration |
| `register_session` | no | Create or attach shell/TUI state | Only shell can own ZLE |
| `focus` | yes | Mark a matching TTY as active | TTY must match the session |
| `command_started` | yes | Record bounded in-flight command metadata | Local analysis may emit a hint |
| `command_finished` | yes | Complete, bound, and analyze a command | Late erased FINISH frames stay erased |
| `completion` | yes | Request typed completion for a ZLE snapshot | Newer completion cancels older one |
| `risk_lens` | yes | Inspect the current buffer locally | No provider call |
| `cancel` | yes | Cancel one request by ID | Cancellation is session-scoped |
| `chat` | yes | Ask a terminal-inline or TUI question | Redacted; stream events return to origin |
| `context` | yes | Read bounded session context | Also subscribes the client to notifications |
| `airlock` | yes | Inspect, seal, or open the session's provider boundary | Seal atomically blocks new provider work and cancels active work |
| `checkpoint` | yes | Start, resolve, inspect, or clear a marker | Marker never enters provider prompts |
| `data` | varies | Inventory or clear typed local-data scopes | Inventory contains counts, never content |
| `insert_buffer` | yes | Ask the daemon to hand a visible proposal to ZLE | Daemon recomputes safety classification |
| `disconnect` | optional | Detach the current connection/session route | Retention policy still applies |
| `ping` | no | Liveness check | No session mutation |
| `shutdown` | no | Graceful daemon stop | Local administrative operation |

Field-level definitions, defaults, and serialization names are intentionally not duplicated here;
read the public structs adjacent to `RequestBody` before implementing a client.

## Results, events, and routing

Responses are point-to-point: they return on the connection that sent the request.

Events have four routing classes:

1. **Request events** — streaming chat deltas/done/failure and request cancellation return only to
   the originating connection.
2. **Session events** — hints and data-clear notifications go to the owning shell and clients that
   subscribed to that session through context or chat.
3. **Shell mutation events** — insert-buffer proposals go only to the current shell owner whose
   negotiated capabilities permit insertion. They never go to a TUI observer.
4. **Observer metadata events** — live Privacy Receipts and Session Airlock changes go only to
   non-shell clients subscribed to the session. They are deliberately suppressed for shell clients
   so asynchronous metadata never disturbs ZLE or terminal output. The synchronous response to an
   explicit Airlock command still returns to its caller.

Current event bodies are `hint`, `completion`, `chat_delta`, `chat_done`, `chat_failed`,
`insert_buffer`, `request_cancelled`, `data_cleared`, `airlock_changed`, `privacy_receipt`, and
`session_closed`.

The bounded outbound queue applies backpressure. A disconnected or slow observer must not block the
shell hook or cause work to be executed elsewhere.

## Session lifecycle

A shell registers with a TTY, process ID, cwd, shell name, terminal name, and an allowlisted
environment snapshot. The daemon owns the association between the session and connection. A TUI
using a session ID becomes an observer; it does not replace the owner.

Commands use independent `command_id` values so start and finish frames can be paired. Context is
bounded by command count, per-command output characters, and total characters. Disconnected
sessions use an LRU limit and TTL; connected sessions are not evicted.

Active completion/chat/analysis work has a cancellation token. Disconnect, replacement requests,
and data clearing cancel the relevant work. A completion response may be valid at the protocol
level but ZLE still rejects it if the user's current buffer differs from the original snapshot.

Every new session begins with provider access enabled. The Session Airlock flag is retained only in
daemon memory and is isolated from every other session. Re-registering or clearing data for an
existing session preserves the flag; a daemon restart or session eviction removes it, so a later
session starts open again.

## Security and privacy contract

- The socket is a local transport, not a reason to trust input. The daemon validates session
  ownership and request shape.
- Only `LANG`, `LC_ALL`, `LC_CTYPE`, `TERM`, `COLORTERM`, `VIRTUAL_ENV`, and
  `CONDA_DEFAULT_ENV` can enter retained environment context. Values are control-free and bounded.
- Command/output/path/chat content is redacted at the provider boundary. Risk Lens, Source Cards,
  Failure Fingerprints, Environment Drift, checkpoints, and data inventory run locally.
- Provider responses are untrusted input: structured outputs are validated, terminal controls are
  stripped, cursor offsets are clamped, and command proposals are rescanned.
- Error responses must describe a safe error kind without echoing credentials, HTTP bodies, prompts,
  terminal history, or private paths unnecessarily.
- A shell buffer proposal is never an execution instruction. Clients display or insert it and leave
  Enter untouched.

### Live Privacy Receipt

Every completion, analysis, or chat operation that reaches the provider abstraction emits one
`privacy_receipt` after that attempt finishes. Local-only analysis and Risk Lens do not emit one
because no provider-bound request exists. Its fields are content-free metadata:

| Field | Meaning |
|---|---|
| `purpose` | `completion`, `analysis`, or `chat` |
| `outcome` | `succeeded`, `failed`, `cancelled`, or `local_fallback` |
| `payload_chars` | Unicode scalar count of the JSON-serialized application request after redaction, before model/provider configuration and HTTP framing |
| `payload_items` | Completion context entries, analysis context records, or chat messages in that request |
| `redactions` | Aggregate number of secret-shaped spans replaced; no rule or match is retained |
| `redaction_enabled` | The actual provider-bound redaction setting for the request |
| `elapsed_ms` | Time spent awaiting the provider attempt, including stream consumption |

The event never carries the prompt, response, matched value, redaction rule, model, endpoint, path,
or history content. It is not appended to chat history, logs, capsules, failure memory, or another
persistent store. A Coach window must already be subscribed to observe it; the daemon does not
replay old receipts. It proves what this application prepared at its provider boundary, not how an
external provider stores or processes a request.

### Session Airlock

`airlock` has three actions: `status`, `seal`, and `open`. Its response and change event carry only:

| Field | Meaning |
|---|---|
| `provider_access_enabled` | `false` while the session is sealed; `true` while open |
| `cancelled_requests` | Number of active provider requests cancelled by this operation |

`seal` sets the flag and removes/cancels all active completion, analysis, and chat requests under
the same state lock. A provider request cannot race through after the seal. New provider-bound
operations fail with the stable, non-retryable `airlock_sealed` error before a prompt or chat entry
is prepared. Already-started attempts finish their normal cancellation path and may emit a
content-free Privacy Receipt with outcome `cancelled`.

The boundary does not disable local Risk Lens, Source Cards, failure recognition, Environment
Drift, data controls, context, or diagnostics. Automatic failure analysis falls back to its local
result and emits no Privacy Receipt when the seal prevented provider access entirely. `open` allows
future provider requests but never sends one by itself. Data clearing does not change the flag.
The flag is intentionally memory-only: manually restarting the daemon resets all sessions, and a
new or re-created session starts open. The TUI displays the state continuously; shells never receive
the asynchronous `airlock_changed` observer event.

## Compatibility rules

An additive field is compatible when old readers can ignore it and new readers provide a serde
default when it is absent. Adding an enum variant can still break exhaustive clients, so document
the minimum compatible release.

Protocol v3 is the minimum version for `privacy_receipt`. Protocol v4 adds `airlock`,
`airlock_changed`, and the default-true provider-access field in context/inventory. The enum variants
required a bump so exhaustive v3 clients fail the handshake instead of misinterpreting the stream.

Bump `PROTOCOL_VERSION` when changing an existing serialized field name or type, removing a field or
variant, changing envelope/tag shape, changing identifier meaning, or making previously optional
data mandatory. Keep the rejection error explicit; do not silently reinterpret another version.

For a protocol change, tests must cover:

- JSON serialize/deserialize round trips;
- absent optional fields from an older client;
- Zsh encode/decode if the shell uses the operation;
- a real Unix-socket integration path;
- wrong session, wrong client kind, cancellation, and disconnect where applicable;
- terminal-control and size boundaries for displayed or retained strings.

## Minimal contributor workflow

```zsh
cargo test -p aicoach-ipc --locked
cargo test -p aicoach-daemon --test ipc_integration --locked
zsh scripts/test-zsh-integration.zsh
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Then run the complete gate in [CONTRIBUTING.md](../CONTRIBUTING.md). For architectural placement and
extension recipes, see [ARCHITECTURE.md](ARCHITECTURE.md).

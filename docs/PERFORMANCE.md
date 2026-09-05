# Performance Budgets

AI Terminal Coach sits inside an interactive shell, so latency is a product boundary rather than a
cleanup task. Every budget below is measured from shipped code and fails CI when exceeded.

The current Apple Silicon dashboard is attached to every `main` [CI run
summary](https://github.com/BlueKiteCoder/ai-terminal-coach/actions/workflows/ci.yml). Pull requests
that affect release inputs also publish native Apple Silicon and Intel tables in the [Release
workflow](https://github.com/BlueKiteCoder/ai-terminal-coach/actions/workflows/release.yml).

## Guardrails

| Metric | Budget | Measurement |
|---|---:|---|
| Zsh integration source | 50 ms | One clean source with widget/hook registration stubbed |
| Synchronous `preexec` work | 10 ms average | 2,000 in-process calls; socket delivery stubbed |
| Synchronous `precmd` work | 10 ms average | 2,000 in-process calls; socket delivery stubbed |
| `aicoach --version` startup | 100 ms p50 | Median of 40 warmed process launches |
| `aicoachd --version` startup | 100 ms p50 | Median of 40 warmed process launches |
| `aicoach-ui --version` startup | 100 ms p50 | Median of 40 warmed process launches |
| Combined stripped release executables | 32 MiB | CLI, daemon, TUI, plus hotkey helper when present |

These are regression ceilings, not marketing claims. A shared CI runner varies with host load. The
budgets intentionally leave room for that noise while still catching order-of-magnitude regressions.
The process-startup probe measures executable loading and argument parsing with `--version`; it does
not claim to measure daemon socket readiness, terminal automation, network latency, or provider
response time.

## Run locally

```zsh
cargo build --release --workspace --locked
zsh scripts/benchmark-product.zsh
zsh scripts/benchmark-product.zsh --markdown
zsh scripts/benchmark-product.zsh --json
```

To measure another release directory without moving binaries:

```zsh
AICOACH_BENCHMARK_BIN_DIR=/path/to/release/bin \
  zsh scripts/benchmark-product.zsh --json
```

The JSON result has `schema_version: 1`, the native architecture, budgets, measurements, per-binary
bytes, a combined byte count, and a final `passed` boolean. The command still exits nonzero when any
budget fails, so dashboards cannot accidentally turn a regression into a green result.

## Why socket and provider time are separate

Zsh hooks never wait for either one. They serialize bounded metadata to a local socket and return;
daemon analysis and provider work are asynchronous. Socket routing, cancellation, streaming, and
provider timeouts are therefore covered by deterministic integration tests instead of being folded
into a noisy “keypress to AI answer” number.

When changing hook code, run `benchmark-zsh-hooks.zsh` before and after the change and include both
numbers in the pull request. When adding a dependency to a binary, include the product dashboard and
explain a material size or startup increase.

#!/bin/zsh
set -euo pipefail
export LC_NUMERIC=C

format=${1:-human}
case $format in
  human|--json|--markdown) ;;
  *) print -u2 -- 'usage: benchmark-product.zsh [--json|--markdown]'; exit 2 ;;
esac

root=${0:A:h:h}
bin_dir=${AICOACH_BENCHMARK_BIN_DIR:-$root/target/release}
typeset -a required_binaries
required_binaries=(aicoach aicoachd aicoach-ui)
for name in $required_binaries; do
  [[ -x "$bin_dir/$name" ]] || {
    print -u2 -- "missing release executable: $bin_dir/$name"
    print -u2 -- 'run cargo build --release --workspace --locked first'
    exit 1
  }
done

zmodload zsh/datetime

measure_startup() {
  local binary=$1
  local iterations=${2:-40}
  local -a samples sorted
  local -F 9 started elapsed_ms
  integer index

  repeat 3 "$binary" --version >/dev/null
  for (( index = 1; index <= iterations; index++ )); do
    started=$EPOCHREALTIME
    "$binary" --version >/dev/null
    elapsed_ms=$(( (EPOCHREALTIME - started) * 1000.0 ))
    samples+=("$elapsed_ms")
  done
  sorted=("${(@f)$(printf '%.9f\n' $samples | LC_ALL=C /usr/bin/sort -n)}")
  REPLY=${sorted[$(( (iterations + 1) / 2 ))]}
}

IFS=$'\t' read -r source_ms preexec_ms precmd_ms <<< \
  "$(zsh "$root/scripts/benchmark-zsh-hooks.zsh" --tsv)"
typeset -F 6 source_ms preexec_ms precmd_ms

measure_startup "$bin_dir/aicoach"
typeset -F 6 cli_startup_ms=$REPLY
measure_startup "$bin_dir/aicoachd"
typeset -F 6 daemon_startup_ms=$REPLY
measure_startup "$bin_dir/aicoach-ui"
typeset -F 6 tui_startup_ms=$REPLY

typeset -A binary_sizes
integer total_binary_bytes=0
for name in $required_binaries; do
  bytes=$(/usr/bin/stat -f '%z' "$bin_dir/$name")
  binary_sizes[$name]=$bytes
  (( total_binary_bytes += bytes ))
done
if [[ -x "$bin_dir/aicoach-hotkey" ]]; then
  bytes=$(/usr/bin/stat -f '%z' "$bin_dir/aicoach-hotkey")
  binary_sizes[aicoach-hotkey]=$bytes
  (( total_binary_bytes += bytes ))
fi

typeset -F 3 total_binary_mib=$(( total_binary_bytes / 1048576.0 ))
typeset -F 1 source_budget_ms=50.0
typeset -F 1 hook_budget_ms=10.0
typeset -F 1 startup_budget_ms=100.0
integer binary_budget_bytes=$(( 32 * 1024 * 1024 ))
integer failed=0

(( source_ms > 0.0 && preexec_ms > 0.0 && precmd_ms > 0.0 )) || failed=1
(( cli_startup_ms > 0.0 && daemon_startup_ms > 0.0 && tui_startup_ms > 0.0 )) || failed=1
(( total_binary_bytes > 0 )) || failed=1

within_budget() {
  (( $1 <= $2 ))
}

within_budget $source_ms $source_budget_ms || failed=1
within_budget $preexec_ms $hook_budget_ms || failed=1
within_budget $precmd_ms $hook_budget_ms || failed=1
within_budget $cli_startup_ms $startup_budget_ms || failed=1
within_budget $daemon_startup_ms $startup_budget_ms || failed=1
within_budget $tui_startup_ms $startup_budget_ms || failed=1
(( total_binary_bytes <= binary_budget_bytes )) || failed=1

status_for() {
  if within_budget $1 $2; then
    REPLY='pass'
  else
    REPLY='FAIL'
  fi
}

case $format in
  --json)
    printf '{"schema_version":1,"architecture":"%s","startup_iterations":40,' "$(uname -m)"
    printf '"budgets":{"source_ms":%.1f,"hook_ms":%.1f,"startup_p50_ms":%.1f,"total_binary_bytes":%d},' \
      "$source_budget_ms" "$hook_budget_ms" "$startup_budget_ms" "$binary_budget_bytes"
    printf '"measurements":{"source_ms":%.3f,"preexec_ms":%.3f,"precmd_ms":%.3f,' \
      "$source_ms" "$preexec_ms" "$precmd_ms"
    printf '"aicoach_startup_p50_ms":%.3f,"aicoachd_startup_p50_ms":%.3f,"aicoach_ui_startup_p50_ms":%.3f},' \
      "$cli_startup_ms" "$daemon_startup_ms" "$tui_startup_ms"
    printf '"binaries_bytes":{"aicoach":%d,"aicoachd":%d,"aicoach-ui":%d' \
      "$binary_sizes[aicoach]" "$binary_sizes[aicoachd]" "$binary_sizes[aicoach-ui]"
    [[ -n ${binary_sizes[aicoach-hotkey]:-} ]] && \
      printf ',"aicoach-hotkey":%d' "$binary_sizes[aicoach-hotkey]"
    printf ',"total":%d},"passed":%s}\n' \
      "$total_binary_bytes" "$([[ $failed == 0 ]] && print true || print false)"
    ;;
  --markdown)
    print "### Performance budget — $(uname -m)"
    print
    print '| Metric | Measured | Budget | Result |'
    print '|---|---:|---:|:---:|'
    status_for $source_ms $source_budget_ms
    printf '| Zsh integration source | %.3f ms | ≤ %.0f ms | %s |\n' "$source_ms" "$source_budget_ms" "$REPLY"
    status_for $preexec_ms $hook_budget_ms
    printf '| Zsh `preexec` average | %.3f ms | ≤ %.0f ms | %s |\n' "$preexec_ms" "$hook_budget_ms" "$REPLY"
    status_for $precmd_ms $hook_budget_ms
    printf '| Zsh `precmd` average | %.3f ms | ≤ %.0f ms | %s |\n' "$precmd_ms" "$hook_budget_ms" "$REPLY"
    status_for $cli_startup_ms $startup_budget_ms
    printf '| `aicoach --version` p50 | %.3f ms | ≤ %.0f ms | %s |\n' "$cli_startup_ms" "$startup_budget_ms" "$REPLY"
    status_for $daemon_startup_ms $startup_budget_ms
    printf '| `aicoachd --version` p50 | %.3f ms | ≤ %.0f ms | %s |\n' "$daemon_startup_ms" "$startup_budget_ms" "$REPLY"
    status_for $tui_startup_ms $startup_budget_ms
    printf '| `aicoach-ui --version` p50 | %.3f ms | ≤ %.0f ms | %s |\n' "$tui_startup_ms" "$startup_budget_ms" "$REPLY"
    status_for $total_binary_bytes $binary_budget_bytes
    printf '| Combined release executables | %.3f MiB | ≤ 32 MiB | %s |\n' "$total_binary_mib" "$REPLY"
    print
    print 'Source is the median of 9 loads; startup is the median of 40 warmed process launches. Hooks use 2,000 in-process iterations.'
    ;;
  human)
    printf 'AI Terminal Coach performance (%s)\n' "$(uname -m)"
    printf '  Zsh source:           %.3f ms (budget %.0f ms)\n' "$source_ms" "$source_budget_ms"
    printf '  preexec average:      %.3f ms (budget %.0f ms)\n' "$preexec_ms" "$hook_budget_ms"
    printf '  precmd average:       %.3f ms (budget %.0f ms)\n' "$precmd_ms" "$hook_budget_ms"
    printf '  aicoach startup p50:  %.3f ms (budget %.0f ms)\n' "$cli_startup_ms" "$startup_budget_ms"
    printf '  aicoachd startup p50: %.3f ms (budget %.0f ms)\n' "$daemon_startup_ms" "$startup_budget_ms"
    printf '  aicoach-ui p50:       %.3f ms (budget %.0f ms)\n' "$tui_startup_ms" "$startup_budget_ms"
    printf '  release executables:  %.3f MiB (budget 32 MiB)\n' "$total_binary_mib"
    [[ $failed == 0 ]] && print 'performance budgets: ok'
    ;;
esac

if (( failed != 0 )); then
  print -u2 -- 'one or more performance budgets were exceeded'
  exit 1
fi

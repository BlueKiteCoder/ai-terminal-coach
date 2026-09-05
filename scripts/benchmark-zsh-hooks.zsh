#!/bin/zsh
set -euo pipefail
export LC_NUMERIC=C

format=${1:-human}
case $format in
  human|--json|--tsv) ;;
  *) print -u2 -- 'usage: benchmark-zsh-hooks.zsh [--json|--tsv]'; exit 2 ;;
esac

typeset -g AICOACH_TEST_MODE=1
typeset -g benchmark_settings_dir=$(mktemp -d "${TMPDIR:-/tmp}/aicoach-benchmark.XXXXXX")
trap 'rm -rf -- "$benchmark_settings_dir"' EXIT
typeset -g AICOACH_SETTINGS_FILE=$benchmark_settings_dir/keybindings.zsh
typeset -g AICOACH_SETTINGS_VERSION_FILE=$benchmark_settings_dir/keybindings.version
builtin print -r -- '1' >| $AICOACH_SETTINGS_VERSION_FILE
zle() { return 0 }
bindkey() { return 0 }
add-zsh-hook() { return 0 }
add-zle-hook-widget() { return 0 }

zmodload zsh/datetime
typeset -a source_samples source_sorted
typeset -F source_started source_elapsed
repeat 9; do
  source_started=$EPOCHREALTIME
  source "${0:A:h:h}/shell/aicoach.zsh"
  source_elapsed=$(( (EPOCHREALTIME - source_started) * 1000.0 ))
  source_samples+=("$source_elapsed")
done
source_sorted=("${(@f)$(printf '%.9f\n' $source_samples | LC_ALL=C /usr/bin/sort -n)}")
typeset -F source_ms=${source_sorted[5]}

# Measure only synchronous hook work. Socket delivery and all AI work are
# deliberately asynchronous and covered by the daemon IPC integration tests.
_aicoach_send() { return 0 }

_bench_preexec() {
  _aicoach_preexec 'git status --short'
}

_bench_precmd() {
  typeset -g AICOACH_COMMAND_ID='00000000-0000-4000-8000-000000000001'
  _aicoach_now_ms
  typeset -gF AICOACH_COMMAND_STARTED=$REPLY
  _aicoach_precmd
}

measure_hook() {
  local function_name=$1 iterations=${2:-2000}
  typeset -F started=$EPOCHREALTIME
  repeat $iterations "$function_name" >/dev/null
  typeset -F elapsed_ms=$(( (EPOCHREALTIME - started) * 1000.0 ))
  typeset -F average_ms=$(( elapsed_ms / iterations ))
  REPLY=$average_ms
}

measure_hook _bench_preexec
typeset -F preexec_ms=$REPLY
measure_hook _bench_precmd
typeset -F precmd_ms=$REPLY

typeset -gi failed=0
(( source_ms > 0.0 && preexec_ms > 0.0 && precmd_ms > 0.0 )) || {
  print -u2 -- 'hook benchmark produced a non-positive measurement'
  failed=1
}
(( source_ms <= 50.0 )) || {
  printf >&2 'source exceeded the 50 ms integration-load budget: %.3f ms\n' "$source_ms"
  failed=1
}
for measurement in preexec_ms precmd_ms; do
  (( ${(P)measurement} <= 10.0 )) || {
    printf >&2 '%s exceeded the 10 ms local-hook budget: %.3f ms\n' \
      "${measurement%_ms}" "${(P)measurement}"
    failed=1
  }
done

case $format in
  --json)
    printf '{"schema_version":1,"iterations":2000,"source_ms":%.3f,"preexec_ms":%.3f,"precmd_ms":%.3f,"passed":%s}\n' \
      "$source_ms" "$preexec_ms" "$precmd_ms" "$([[ $failed == 0 ]] && print true || print false)"
    ;;
  --tsv)
    printf '%.6f\t%.6f\t%.6f\n' "$source_ms" "$preexec_ms" "$precmd_ms"
    ;;
  human)
    printf '%-12s %.3f ms\n' 'source' "$source_ms"
    printf '%-12s average %.3f ms (%d iterations)\n' 'preexec' "$preexec_ms" 2000
    printf '%-12s average %.3f ms (%d iterations)\n' 'precmd' "$precmd_ms" 2000
    [[ $failed == 0 ]] && print 'zsh hook performance: ok'
    ;;
esac
(( failed == 0 ))

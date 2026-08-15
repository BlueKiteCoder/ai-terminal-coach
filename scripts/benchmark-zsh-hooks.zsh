#!/bin/zsh
set -euo pipefail

typeset -g AICOACH_TEST_MODE=1
zle() { return 0 }
bindkey() { return 0 }
add-zsh-hook() { return 0 }
add-zle-hook-widget() { return 0 }

zmodload zsh/datetime
typeset -F source_started=$EPOCHREALTIME
source "${0:A:h:h}/shell/aicoach.zsh"
typeset -F source_ms=$(( (EPOCHREALTIME - source_started) * 1000.0 ))

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

benchmark_hook() {
  local name=$1 function_name=$2 iterations=${3:-2000}
  typeset -F started=$EPOCHREALTIME
  repeat $iterations "$function_name" >/dev/null
  typeset -F elapsed_ms=$(( (EPOCHREALTIME - started) * 1000.0 ))
  typeset -F average_ms=$(( elapsed_ms / iterations ))
  printf '%-12s average %.3f ms (%d iterations)\n' "$name" "$average_ms" "$iterations"
  (( average_ms < 10.0 )) || {
    print -u2 -- "$name exceeded the 10 ms local-hook budget"
    return 1
  }
}

printf '%-12s %.3f ms\n' 'source' "$source_ms"
benchmark_hook preexec _bench_preexec
benchmark_hook precmd _bench_precmd
print 'zsh hook performance: ok'

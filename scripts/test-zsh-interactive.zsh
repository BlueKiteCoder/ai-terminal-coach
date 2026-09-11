#!/bin/zsh
set -euo pipefail

# Exercise the shipped bindings inside a real ZLE session. The existing unit
# probe stubs `bindkey` and `zle`, so it cannot catch a binding that is present
# on paper but never dispatches when Escape-prefixed bytes reach a terminal.
zmodload zsh/datetime
zmodload zsh/zpty

typeset -gr test_root=${0:A:h:h}
typeset -gr integration=$test_root/shell/aicoach.zsh
typeset -gr test_home=$(mktemp -d "${TMPDIR:-/tmp}/aicoach-zle-test.XXXXXX")
typeset -g transcript=""
typeset -g child="aicoach-zle-$RANDOM-$$"
typeset -g server_pid=""
typeset -gr test_socket=$test_home/state/run/aicoach.sock
typeset -gr server_ready=$test_home/server-ready

cleanup() {
  zpty -d "$child" 2>/dev/null || true
  if [[ -n $server_pid ]]; then
    kill -TERM "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf -- "$test_home"
}
trap cleanup EXIT HUP INT TERM

fail() {
  print -u2 -r -- "FAIL: $1"
  print -u2 -r -- "interactive ZLE transcript: ${(qqq)transcript}"
  exit 1
}

read_until() {
  local expected=$1 chunk
  local -F deadline=$(( EPOCHREALTIME + 5.0 ))
  transcript=""
  while (( EPOCHREALTIME < deadline )); do
    if zpty -rt "$child" chunk; then
      transcript+=$chunk
      [[ $transcript == *$expected* ]] && return 0
    else
      sleep 0.02
    fi
  done
  return 1
}

# HOME and both settings paths are isolated so this test never reads the
# developer's shell configuration. PATH deliberately excludes project builds,
# preventing the integration from starting a real daemon.
zpty -b "$child" \
  "env -i HOME=${(q)test_home} PATH=/usr/bin:/bin TERM=xterm-256color LANG=en_US.UTF-8 PS1='AICOACH-BOOT> ' /bin/zsh -dfi"
read_until 'AICOACH-BOOT> ' || fail 'interactive Zsh did not reach its first prompt'

# A local Unix-socket fixture consumes the real REGISTER/FOCUS/request frames
# and returns daemon-shaped LENS and COMPLETE frames. This keeps the test
# provider-free while exercising zsocket, zle -F, sysread, buffering, dispatch,
# and the final ZLE mutation end to end.
mkdir -p "$test_socket:h"
(
  zmodload zsh/net/socket
  zmodload zsh/system
  zsocket -l "$test_socket"
  typeset listener_fd=$REPLY client_fd="" read_buffer="" chunk line normalized response
  typeset -a fields
  builtin print -r -- ready >| "$server_ready"
  zsocket -a "$listener_fd"
  client_fd=$REPLY
  while sysread -i "$client_fd" -s 65536 chunk 2>/dev/null; do
    read_buffer+=$chunk
    while [[ $read_buffer == *$'\n'* ]]; do
      line=${read_buffer%%$'\n'*}
      read_buffer=${read_buffer#*$'\n'}
      normalized=${line//$'\t'/$'\n'}
      fields=("${(@f)normalized}")
      response=""
      case ${fields[2]:-} in
        LENS)
          response=$'LENS\t'${fields[3]}$'\t'${fields[4]}$'\thigh\tRisk Lens · HIGH%0AImpact: mock local result'
          ;;
        COMPLETE)
          response=$'COMPLETE\t'${fields[3]}$'\t'${fields[4]}$'\treplace\t18\tdocker ps --format\tCompleted through the socket'
          ;;
      esac
      [[ -z $response ]] || syswrite -o "$client_fd" -- "${response}"$'\n'
    done
  done
) &
server_pid=$!
repeat 100; do
  [[ -e $server_ready ]] && break
  sleep 0.02
done
[[ -e $server_ready ]] || fail 'local Unix-socket fixture did not start'

typeset setup="
PS1='AICOACH-PTY> '
RPS1=''
KEYTIMEOUT=1
typeset -gx AICOACH_HOME=${(q)test_home}/state
typeset -gx AICOACH_SETTINGS_FILE=${(q)test_home}/keybindings.zsh
typeset -gx AICOACH_SETTINGS_VERSION_FILE=${(q)test_home}/keybindings.version
source ${(q)integration}
_aicoach_test_state_widget() {
  zle -I
  print -r -- \"AICOACH-STATE:\$BUFFER:\$AICOACH_COMPLETION_ID:\$AICOACH_RISK_LENS_ID:\$AICOACH_STATUS_REQUEST_ID\"
  zle redisplay
}
zle -N aicoach-test-state _aicoach_test_state_widget
bindkey -M emacs '^X^X' aicoach-test-state
print -r -- AICOACH-PTY-READY
"
zpty -w "$child" "$setup"
read_until 'AICOACH-PTY-READY' || fail 'shell integration did not load'

# Send the bytes produced by Option+R in Meta mode, not a widget name.
zpty -w -n "$child" 'git status'
zpty -w -n "$child" $'\er'
read_until 'Risk Lens · HIGH' || fail 'ESC+r did not dispatch and display the Risk Lens result'
[[ $transcript == *'Inspecting command impact locally…'* ]] ||
  fail 'ESC+r did not publish its immediate Risk Lens status'
zpty -w -n "$child" $'\C-x\C-x'
read_until 'AICOACH-STATE:git status:::' || fail 'Risk Lens response did not clear its busy request state'

# Cancel the still-edited line, type a completion prefix, then send the bytes
# produced by Option+Tab. The fake sender intentionally leaves this request in
# flight so the transient completion status remains observable.
zpty -w -n "$child" $'\C-c'
read_until 'AICOACH-PTY> ' || fail 'interactive Zsh did not recover after Risk Lens'
zpty -w -n "$child" 'docker ps --forma'
zpty -w -n "$child" $'\e\t'
read_until 'docker ps --format' || fail 'ESC+Tab did not apply the socket completion to the ZLE buffer'
[[ $transcript == *'Generating completion…'* ]] ||
  fail 'ESC+Tab did not publish its immediate completion status'
zpty -w -n "$child" $'\C-x\C-x'
read_until 'AICOACH-STATE:docker ps --format:::' || fail 'completion response did not clear its busy request state'

print -r -- 'interactive ZLE shortcut tests: ok'

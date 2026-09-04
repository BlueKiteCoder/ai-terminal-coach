#!/bin/zsh
set -euo pipefail

typeset -g test_failed=0
assert_eq() {
  if [[ $1 != $2 ]]; then
    print -u2 -r -- "FAIL: expected <$2>, got <$1>"
    test_failed=1
  fi
}

# Load only function definitions by stubbing hook/widget registration.
typeset -g AICOACH_TEST_MODE=1
typeset -g last_zle_message=""
zle() {
  case ${1:-} in
    aicoach-apply-pending) _aicoach_apply_pending_widget ;;
    aicoach-apply-completion) _aicoach_apply_completion_widget ;;
    aicoach-apply-chat-display) _aicoach_apply_chat_display_widget ;;
    -M) last_zle_message=${2:-} ;;
  esac
  return 0
}
typeset -gA test_bindings
bindkey() {
  if [[ ${1:-} == -M ]]; then
    local keymap=$2 sequence=$3 widget=$4
    local binding_key="${keymap}:${sequence}"
    test_bindings[$binding_key]=$widget
  fi
  return 0
}
add-zsh-hook() { return 0 }
add-zle-hook-widget() { return 0 }
print() {
  [[ ${1:-} == -u2 ]] && builtin print "$@" || return 0
}
source "${0:A:h:h}/shell/aicoach.zsh"

assert_eq "$AICOACH_LANGUAGE" 'en-US'
_aicoach_text thinking
assert_eq "$REPLY" 'Thinking…'
typeset -g AICOACH_LANGUAGE='zh-CN'
_aicoach_text thinking
assert_eq "$REPLY" '正在思考…'
typeset -g AICOACH_LANGUAGE='en-US'

meta_chat_binding=$'emacs:\e/'
native_chat_binding='viins:÷'
assert_eq "${test_bindings[$meta_chat_binding]:-}" 'aicoach-chat'
assert_eq "${test_bindings[$native_chat_binding]:-}" 'aicoach-chat'
meta_lens_binding=$'emacs:\er'
native_lens_binding='viins:®'
assert_eq "${test_bindings[$meta_lens_binding]:-}" 'aicoach-risk-lens'
assert_eq "${test_bindings[$native_lens_binding]:-}" 'aicoach-risk-lens'

_aicoach_encode $'hello\t世界\n100%'
encoded=$REPLY
assert_eq "$encoded" 'hello%09世界%0A100%25'
_aicoach_decode "$encoded"
assert_eq "$REPLY" $'hello\t世界\n100%'

typeset -gx LANG='zh_CN.UTF-8'
typeset -gx VIRTUAL_ENV=$'/tmp/venv\nTERM=forged'
typeset -gx UNLISTED_PRIVATE_VALUE='must-not-be-captured'
_aicoach_environment_snapshot
environment_snapshot=$REPLY
[[ $environment_snapshot == *$'LANG=zh_CN.UTF-8\n'* ]] || {
  builtin print -u2 -r -- 'FAIL: LANG missing from allowlisted environment snapshot'
  test_failed=1
}
[[ $environment_snapshot == *$'VIRTUAL_ENV=/tmp/venv TERM=forged\n'* ]] || {
  builtin print -u2 -r -- 'FAIL: environment controls were not neutralized'
  test_failed=1
}
[[ $environment_snapshot != *'UNLISTED_PRIVATE_VALUE'* && $environment_snapshot != *'must-not-be-captured'* ]] || {
  builtin print -u2 -r -- 'FAIL: secret-like environment variable was captured'
  test_failed=1
}

_aicoach_safe_display $'safe\e]52;c;owned\a\nnext'
assert_eq "$REPLY" 'safe]52;c;owned next'
_aicoach_safe_multiline_display $'第一行\e[31m\n\t第二行\e[0m'
assert_eq "$REPLY" $'第一行[31m\n  第二行[0m'
_aicoach_safe_buffer $'echo ok\nrm -rf /' && test_failed=1 || true

# zsh uses plain `name+=value` for an existing scalar. Combining `typeset`
# with `name+=` is a runtime error and previously broke every socket callback.
sysread() {
  local target=${@[-1]}
  eval "$target=\$'PONG\\n'"
  return 0
}
typeset -g AICOACH_READ_BUFFER=""
_aicoach_socket_ready 99
assert_eq "$AICOACH_READ_BUFFER" ""
unfunction sysread

_aicoach_local_danger 'rm -rf /'
assert_eq "$?" 0
_aicoach_local_danger 'rm -rf *'
assert_eq "$?" 0
_aicoach_local_danger "echo 'rm -rf /'" && test_failed=1 || true
_aicoach_local_danger 'git reset --hard'
assert_eq "$?" 0
_aicoach_local_danger "echo 'git reset --hard'" && test_failed=1 || true
_aicoach_local_danger 'git status' && test_failed=1 || true

typeset -g BUFFER='docker ps --forma'
typeset -g CURSOR=${#BUFFER}
typeset -g AICOACH_COMPLETION_ID=req-1
typeset -g AICOACH_COMPLETION_SNAPSHOT=$BUFFER
_aicoach_handle_line $'COMPLETE\t'$AICOACH_SESSION_ID$'\treq-1\treplace\t18\tdocker ps --format\t修正参数'
assert_eq "$BUFFER" 'docker ps --format'
assert_eq "$CURSOR" '18'
assert_eq "$last_zle_message" '[AI Coach] 修正参数'

typeset -g BUFFER='echo'
typeset -g CURSOR=${#BUFFER}
typeset -g AICOACH_COMPLETION_ID=req-2
typeset -g AICOACH_COMPLETION_SNAPSHOT=$BUFFER
_aicoach_handle_line $'COMPLETE\t'$AICOACH_SESSION_ID$'\treq-2\treplace\t8\trm -rf /\tbad'
assert_eq "$BUFFER" 'echo'

typeset -g AICOACH_DEFER_INSERT=1
_aicoach_handle_line $'INSERT\t'$AICOACH_SESSION_ID$'\tprintf queued'
assert_eq "$BUFFER" 'echo'
assert_eq "${AICOACH_PENDING_INSERTS[-1]:-}" 'printf queued'
typeset -g AICOACH_DEFER_INSERT=0
_aicoach_apply_pending_inserts
assert_eq "$BUFFER" 'printf queued'
assert_eq "$CURSOR" '13'

typeset -g sent_lens_line=""
_aicoach_send() {
  sent_lens_line=$1
  return 0
}
typeset -g BUFFER='git reset --hard'
typeset -g CURSOR=${#BUFFER}
_aicoach_risk_lens_widget
[[ $sent_lens_line == $'ZSH\tLENS\t'*$'\tgit reset --hard' ]] || {
  builtin print -u2 -r -- 'FAIL: current ZLE buffer was not sent to the local Risk Lens'
  test_failed=1
}
assert_eq "$last_zle_message" '[AI Coach] Inspecting command impact locally…'
lens_request=$AICOACH_RISK_LENS_ID
_aicoach_handle_line $'LENS\t'$AICOACH_SESSION_ID$'\t'$lens_request$'\thigh\tRisk Lens · HIGH%0AImpact: modify Git worktree'
assert_eq "$AICOACH_RISK_LENS_ID" ''
assert_eq "$BUFFER" 'git reset --hard'

typeset -g sent_chat_line=""
_aicoach_send() {
  sent_chat_line=$1
  return 0
}
typeset -g BUFFER='为什么 git push 失败'
typeset -g CURSOR=${#BUFFER}
_aicoach_chat_widget
assert_eq "$BUFFER" ''
assert_eq "$POSTDISPLAY" '[AI Coach] Thinking…'
[[ $sent_chat_line == *$'\t为什么 git push 失败' ]] || {
  builtin print -u2 -r -- 'FAIL: current ZLE buffer was not sent as the chat question'
  test_failed=1
}

stream_request=$AICOACH_CHAT_ID
_aicoach_handle_line $'ANSWER_DELTA\t'$AICOACH_SESSION_ID$'\t'$stream_request$'\t你好'
assert_eq "$AICOACH_CHAT_STREAM_CONTENT" '你好'
assert_eq "$POSTDISPLAY" '[AI Coach] 你好'
assert_eq "$last_zle_message" '[AI Coach] 你好'
_aicoach_handle_line $'ANSWER_DELTA\t'$AICOACH_SESSION_ID$'\t'$stream_request$'\t，世界'
assert_eq "$AICOACH_CHAT_STREAM_CONTENT" '你好，世界'
_aicoach_handle_line $'ANSWER_DELTA\t'$AICOACH_SESSION_ID$'\t'$stream_request$'\t%0A%0A- 列表项%0A  echo ok'
assert_eq "$AICOACH_CHAT_STREAM_CONTENT" $'你好，世界\n\n- 列表项\n  echo ok'
[[ $POSTDISPLAY != *$'\n'* ]] || {
  builtin print -u2 -r -- 'FAIL: persistent stream preview must remain a single line'
  test_failed=1
}

typeset -g last_notice_severity=""
typeset -g last_notice_message=""
_aicoach_notice() {
  last_notice_severity=$1
  last_notice_message=$2
}
_aicoach_handle_line $'ANSWER_DONE\t'$AICOACH_SESSION_ID$'\t'$stream_request
assert_eq "$last_notice_severity" 'info'
assert_eq "$last_notice_message" $'你好，世界\n\n- 列表项\n  echo ok'
assert_eq "$AICOACH_CHAT_ID" ''
assert_eq "$POSTDISPLAY" ''

typeset -g AICOACH_CHAT_ID='stream-failure'
typeset -g AICOACH_CHAT_STREAM_CONTENT='已生成部分'
_aicoach_handle_line $'ERROR\t'$AICOACH_SESSION_ID$'\tstream-failure\tai_unavailable\t连接中断\ttrue'
assert_eq "$last_notice_severity" 'warning'
[[ $last_notice_message == '已生成部分'*'连接中断'* ]] || {
  builtin print -u2 -r -- 'FAIL: interrupted stream did not preserve its partial answer'
  test_failed=1
}

(( test_failed == 0 )) && print 'zsh integration tests: ok'
exit $test_failed

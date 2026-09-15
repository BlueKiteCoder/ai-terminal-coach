#!/bin/zsh
set -euo pipefail

typeset -g test_failed=0
typeset -g last_print=""
assert_eq() {
  if [[ $1 != $2 ]]; then
    print -u2 -r -- "FAIL: expected <$2>, got <$1>"
    test_failed=1
  fi
}

# Load only function definitions by stubbing hook/widget registration.
typeset -g AICOACH_TEST_MODE=1
typeset -g last_zle_message=""
typeset -g test_settings_dir=$(mktemp -d "${TMPDIR:-/tmp}/aicoach-zsh-test.XXXXXX")
trap 'rm -rf -- "$test_settings_dir"' EXIT
typeset -g AICOACH_HOME=$test_settings_dir/state
typeset -g AICOACH_SETTINGS_FILE=$test_settings_dir/keybindings.zsh
typeset -g AICOACH_SETTINGS_VERSION_FILE=$test_settings_dir/keybindings.version
builtin print -r -- '1' >| $AICOACH_SETTINGS_VERSION_FILE
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
    local keymap=$2
    if [[ ${3:-} == -r ]]; then
      local binding_key="${keymap}:${4:-}"
      unset "test_bindings[$binding_key]"
      return 0
    fi
    local sequence=$3
    local binding_key="${keymap}:${sequence}"
    if (( $# == 3 )); then
      [[ -n ${test_bindings[$binding_key]:-} ]] || return 1
      builtin print -r -- "binding ${test_bindings[$binding_key]}"
      return 0
    fi
    local widget=$4
    test_bindings[$binding_key]=$widget
  fi
  return 0
}
add-zsh-hook() { return 0 }
add-zle-hook-widget() { return 0 }
print() {
  if [[ ${1:-} == -u2 ]]; then
    builtin print "$@"
  else
    last_print=${(j: :)@}
  fi
  return 0
}
source "${0:A:h:h}/shell/aicoach.zsh"

assert_eq "$AICOACH_INTEGRATION_VERSION" '4'
assert_eq "$AICOACH_LANGUAGE" 'en-US'
_aicoach_text thinking
assert_eq "$REPLY" 'Thinking…'
typeset -g AICOACH_LANGUAGE='zh-CN'
_aicoach_text thinking
assert_eq "$REPLY" '正在思考…'
typeset -g AICOACH_LANGUAGE='en-US'

meta_completion_emacs=$'emacs:\e\t'
meta_completion_viins=$'viins:\e\t'
assert_eq "${test_bindings[$meta_completion_emacs]:-}" 'aicoach-complete'
assert_eq "${test_bindings[$meta_completion_viins]:-}" 'aicoach-complete'
meta_chat_binding=$'emacs:\e/'
native_chat_binding='viins:÷'
assert_eq "${test_bindings[$meta_chat_binding]:-}" 'aicoach-chat'
assert_eq "${test_bindings[$native_chat_binding]:-}" 'aicoach-chat'
meta_lens_binding=$'emacs:\er'
native_lens_binding='viins:®'
assert_eq "${test_bindings[$meta_lens_binding]:-}" 'aicoach-risk-lens'
assert_eq "${test_bindings[$native_lens_binding]:-}" 'aicoach-risk-lens'

# Generated settings are noticed at the next prompt, old owned bindings are
# removed, and new ones become active without restarting Zsh.
builtin print -l -r -- \
  "typeset -g AICOACH_CONFIG_COMPLETION_KEY=\$'\\eg'" \
  "typeset -g AICOACH_CONFIG_CHAT_KEY=\$'\\ec'" \
  "typeset -g AICOACH_CONFIG_RISK_LENS_KEY=\$'\\el'" \
  "typeset -g AICOACH_CONFIG_TOGGLE_KEY=\$'\\e '" \
  "typeset -g AICOACH_CONFIG_LANGUAGE='en-US'" \
  "typeset -gi AICOACH_CONFIG_SAFETY_ENABLED=1" \
  "typeset -gi AICOACH_CONFIG_INLINE_HINT=1" >| $AICOACH_SETTINGS_FILE
builtin print -r -- '2' >| $AICOACH_SETTINGS_VERSION_FILE
_aicoach_refresh_settings
assert_eq "$AICOACH_COMPLETION_KEY" $'\eg'
assert_eq "$AICOACH_CHAT_KEY" $'\ec'
assert_eq "$AICOACH_RISK_LENS_KEY" $'\el'
assert_eq "${test_bindings[$meta_chat_binding]:-}" ''
new_chat_binding=$'emacs:\ec'
assert_eq "${test_bindings[$new_chat_binding]:-}" 'aicoach-chat'
assert_eq "$AICOACH_SETTINGS_VERSION" '2'

override_binding=$(
  AICOACH_TEST_MODE=1 \
  AICOACH_SETTINGS_FILE=$AICOACH_SETTINGS_FILE \
  AICOACH_SETTINGS_VERSION_FILE=$AICOACH_SETTINGS_VERSION_FILE \
  AICOACH_CHAT_KEY=$'\ex' \
  AICOACH_VERIFY_SCRIPT="${0:A:h:h}/shell/aicoach.zsh" \
    /bin/zsh -dfc 'source "$AICOACH_VERIFY_SCRIPT"; bindkey -M emacs "$AICOACH_CHAT_KEY"'
)
[[ $override_binding == *' aicoach-chat' ]] || {
  builtin print -u2 -r -- 'FAIL: explicit pre-source shortcut override lost precedence'
  test_failed=1
}

# Cross-version sourcing replaces stale widget definitions while preserving
# the original classification of generated versus user-supplied settings.
upgrade_probe=$(
  AICOACH_TEST_MODE=1 \
  AICOACH_SETTINGS_FILE=$AICOACH_SETTINGS_FILE \
  AICOACH_SETTINGS_VERSION_FILE=$AICOACH_SETTINGS_VERSION_FILE \
  AICOACH_VERIFY_SCRIPT="${0:A:h:h}/shell/aicoach.zsh" \
    /bin/zsh -dfc '
      source "$AICOACH_VERIFY_SCRIPT"
      typeset -gx AICOACH_INTEGRATION_VERSION=2
      _aicoach_request_status_begin() { return 77 }
      source "$AICOACH_VERIFY_SCRIPT"
      _aicoach_request_status_begin upgrade-probe ready
      print -r -- "$AICOACH_INTEGRATION_VERSION:$AICOACH_STATUS_REQUEST_ID:$AICOACH_CHAT_KEY_USER_SET"
    '
)
assert_eq "$upgrade_probe" '4:upgrade-probe:0'

# An explicit CLI stop creates this marker before closing the socket. Prompts
# and new terminal tabs must respect it instead of immediately spawning start.
typeset -gx AICOACH_TEST_AUTOSTART_RECORD=$test_settings_dir/autostart-record
typeset -g test_aicoach_stub=$test_settings_dir/aicoach
builtin print -l -r -- \
  '#!/bin/zsh' \
  'builtin print -r -- "$*" >> "$AICOACH_TEST_AUTOSTART_RECORD"' >| $test_aicoach_stub
/bin/chmod 700 $test_aicoach_stub
typeset -ga saved_command_path=("${path[@]}")
path=($test_settings_dir "${path[@]}")
rehash
/bin/mkdir -p ${AICOACH_STOP_FILE:h}
builtin print -r -- manual >| $AICOACH_STOP_FILE
typeset -g AICOACH_LAST_START=0
_aicoach_maybe_start
/bin/sleep 0.05
[[ ! -e $AICOACH_TEST_AUTOSTART_RECORD ]] || {
  builtin print -u2 -r -- 'FAIL: manual stop marker did not suppress daemon auto-start'
  test_failed=1
}
/bin/rm -f $AICOACH_STOP_FILE
typeset -g AICOACH_LAST_START=0
_aicoach_maybe_start
for _ in {1..50}; do
  [[ -e $AICOACH_TEST_AUTOSTART_RECORD ]] && break
  /bin/sleep 0.01
done
assert_eq "$(<$AICOACH_TEST_AUTOSTART_RECORD)" 'start'
path=("${saved_command_path[@]}")
rehash

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
typeset -g socket_handler_calls=0 socket_handler_line=""
functions[_aicoach_handle_line_saved]=$functions[_aicoach_handle_line]
_aicoach_handle_line() {
  (( ++socket_handler_calls ))
  socket_handler_line=$1
  _aicoach_handle_line_saved "$@"
}
sysread() {
  local target=${@[-1]}
  eval "$target=\$'PONG\\n'"
  return 0
}
typeset -g AICOACH_READ_BUFFER=""
_aicoach_socket_ready 99
assert_eq "$AICOACH_READ_BUFFER" ""
assert_eq "$socket_handler_calls" "1"
assert_eq "$socket_handler_line" "PONG"
unfunction sysread
functions[_aicoach_handle_line]=$functions[_aicoach_handle_line_saved]
unfunction _aicoach_handle_line_saved

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
typeset -g AICOACH_COMPLETION_CURSOR=$CURSOR
_aicoach_request_status_begin req-1 'Generating completion…'
# A fast socket response may arrive before the outer key widget has committed
# its BUFFER. The request snapshot remains authoritative in that callback.
typeset -g BUFFER=''
typeset -g CURSOR=0
_aicoach_handle_line $'COMPLETE\t'$AICOACH_SESSION_ID$'\treq-1\treplace\t18\tdocker ps --format\t修正参数'
assert_eq "$BUFFER" 'docker ps --format'
assert_eq "$CURSOR" '18'
assert_eq "$AICOACH_COMPLETION_ID" ''
assert_eq "$last_zle_message" '[AI Coach] 修正参数'

typeset -g BUFFER='echo'
typeset -g CURSOR=${#BUFFER}
typeset -g AICOACH_COMPLETION_ID=req-2
typeset -g AICOACH_COMPLETION_SNAPSHOT=$BUFFER
_aicoach_request_status_begin req-2 'Generating completion…'
_aicoach_handle_line $'COMPLETE\t'$AICOACH_SESSION_ID$'\treq-2\treplace\t8\trm -rf /\tbad'
assert_eq "$BUFFER" 'echo'
assert_eq "$AICOACH_COMPLETION_ID" ''
assert_eq "$last_zle_message" ''

typeset -g AICOACH_DEFER_INSERT=1
_aicoach_handle_line $'INSERT\t'$AICOACH_SESSION_ID$'\tprintf queued\tlow\trecognized\ttrue'
assert_eq "$BUFFER" 'echo'
assert_eq "${AICOACH_PENDING_INSERTS[-1]:-}" 'printf queued'
typeset -g AICOACH_DEFER_INSERT=0
_aicoach_apply_pending_inserts
assert_eq "$BUFFER" 'printf queued'
assert_eq "$CURSOR" '13'
assert_eq "$last_zle_message" '[AI Coach] Insert only · LOW · not executed; review, then press Enter'

typeset -g AICOACH_DEFER_INSERT=1
_aicoach_handle_line $'INSERT\t'$AICOACH_SESSION_ID$'\trm -rf / && company-tool deploy\tcritical\tpartial\ttrue'
typeset -g AICOACH_DEFER_INSERT=0
_aicoach_apply_pending_inserts
assert_eq "$last_zle_message" '[AI Coach] Insert only · CRITICAL · partial coverage · not executed; review, then press Enter'

# A new shell may briefly receive a queued frame from an older daemon during
# an upgrade. Missing classification fields stay conservative, not LOW.
typeset -g AICOACH_DEFER_INSERT=1
_aicoach_handle_line $'INSERT\t'$AICOACH_SESSION_ID$'\tcompany-tool deploy'
typeset -g AICOACH_DEFER_INSERT=0
_aicoach_apply_pending_inserts
assert_eq "$last_zle_message" '[AI Coach] Insert only · UNRATED · unknown command · not executed; review, then press Enter'

typeset -g AICOACH_LANGUAGE='zh-CN'
_aicoach_insert_status high partial false
assert_eq "$last_zle_message" '[AI Coach] 仅插入 · 高风险 · 部分识别 · 破坏性规则已关闭 · 尚未执行；检查后请自行按 Enter'
typeset -g AICOACH_LANGUAGE='en-US'

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
typeset -g BUFFER=''
_aicoach_handle_line $'LENS\t'$AICOACH_SESSION_ID$'\t'$lens_request$'\thigh\tRisk Lens · HIGH%0AImpact: modify Git worktree'
assert_eq "$AICOACH_RISK_LENS_ID" ''
[[ $last_print == *'Risk Lens · HIGH'* ]] || {
  builtin print -u2 -r -- 'FAIL: fast Risk Lens response was discarded with an uncommitted callback buffer'
  test_failed=1
}
assert_eq "$last_zle_message" ''

# Completion/lens errors and cancellation events must release both the request
# record and its transient busy indicator.
typeset -g AICOACH_COMPLETION_ID=req-error
typeset -g AICOACH_COMPLETION_SNAPSHOT='echo err'
_aicoach_request_status_begin req-error 'Generating completion…'
_aicoach_handle_line $'ERROR\t'$AICOACH_SESSION_ID$'\treq-error\tai_unavailable\tprovider unavailable\ttrue'
assert_eq "$AICOACH_COMPLETION_ID" ''
assert_eq "$last_zle_message" ''

typeset -g AICOACH_RISK_LENS_ID=req-cancel
typeset -g AICOACH_RISK_LENS_SNAPSHOT='echo cancel'
_aicoach_request_status_begin req-cancel 'Inspecting command impact locally…'
_aicoach_handle_line $'CANCELLED\t'$AICOACH_SESSION_ID$'\treq-cancel'
assert_eq "$AICOACH_RISK_LENS_ID" ''
assert_eq "$last_zle_message" ''

# A late response from an older request cannot erase the active request's
# status message.
typeset -g AICOACH_COMPLETION_ID=req-new
typeset -g AICOACH_COMPLETION_SNAPSHOT='echo new'
_aicoach_request_status_begin req-new 'Generating completion…'
_aicoach_request_status_end req-old
assert_eq "$last_zle_message" '[AI Coach] Generating completion…'
_aicoach_handle_line $'CANCELLED\t'$AICOACH_SESSION_ID$'\treq-old'
assert_eq "$AICOACH_COMPLETION_ID" 'req-new'
assert_eq "$last_zle_message" '[AI Coach] Generating completion…'
_aicoach_handle_line $'CANCELLED\t'$AICOACH_SESSION_ID$'\treq-new'
assert_eq "$AICOACH_COMPLETION_ID" ''
assert_eq "$last_zle_message" ''

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
# A newer completion owns the shared ZLE message slot. Finishing the older
# chat may restore its POSTDISPLAY, but must leave that completion status alone.
typeset -g AICOACH_COMPLETION_ID='parallel-completion'
typeset -g AICOACH_COMPLETION_SNAPSHOT='echo parallel'
_aicoach_request_status_begin parallel-completion 'Generating completion…'
_aicoach_handle_line $'ANSWER_DONE\t'$AICOACH_SESSION_ID$'\t'$stream_request
assert_eq "$last_notice_severity" 'info'
assert_eq "$last_notice_message" $'你好，世界\n\n- 列表项\n  echo ok'
assert_eq "$AICOACH_CHAT_ID" ''
assert_eq "$POSTDISPLAY" ''
assert_eq "$AICOACH_STATUS_REQUEST_ID" 'parallel-completion'
assert_eq "$last_zle_message" '[AI Coach] Generating completion…'
_aicoach_handle_line $'CANCELLED\t'$AICOACH_SESSION_ID$'\tparallel-completion'
assert_eq "$last_zle_message" ''

typeset -g AICOACH_CHAT_ID='stream-failure'
typeset -g AICOACH_CHAT_STREAM_CONTENT='已生成部分'
_aicoach_handle_line $'ERROR\t'$AICOACH_SESSION_ID$'\tstream-failure\tai_unavailable\t连接中断\ttrue'
assert_eq "$last_notice_severity" 'warning'
[[ $last_notice_message == '已生成部分'*'连接中断'* ]] || {
  builtin print -u2 -r -- 'FAIL: interrupted stream did not preserve its partial answer'
  test_failed=1
}

# Privacy Receipts are observer-only. Even a stray frame from a mismatched
# daemon must be ignored without changing the shell buffer or user message.
typeset -g BUFFER='echo unchanged'
typeset -g last_notice_message='unchanged notice'
_aicoach_handle_line $'PRIVACY_RECEIPT\t'$AICOACH_SESSION_ID$'\treq-privacy\tchat\tsucceeded\t128\t2\t1\ttrue\t20'
assert_eq "$BUFFER" 'echo unchanged'
assert_eq "$last_notice_message" 'unchanged notice'
_aicoach_handle_line $'AIRLOCK\t'$AICOACH_SESSION_ID$'\t\tfalse\t1'
assert_eq "$BUFFER" 'echo unchanged'
assert_eq "$last_notice_message" 'unchanged notice'

if ! /bin/zsh "${0:A:h}/test-zsh-interactive.zsh"; then
  test_failed=1
fi

(( test_failed == 0 )) && print 'zsh integration tests: ok'
exit $test_failed

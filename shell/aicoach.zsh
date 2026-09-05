# AI Terminal Coach shell integration for Zsh.
# Source this file near the end of ~/.zshrc, after prompt/theme plugins.

[[ -o interactive || -n ${AICOACH_TEST_MODE:-} ]] || return 0
if [[ -n ${AICOACH_ZSH_LOADED:-} ]]; then
  # Re-sourcing is an explicit request to pick up generated settings. Older
  # integrations do not have the reload function, so fall through once during
  # an upgrade and replace their definitions in-place.
  if (( $+functions[_aicoach_reload_settings] )); then
    _aicoach_reload_settings
    return 0
  fi
  unset AICOACH_ZSH_LOADED
fi
typeset -g AICOACH_ZSH_LOADED=1
typeset -gx AICOACH_INTEGRATION_VERSION=2

# Remember settings explicitly supplied before this file was sourced. Generated
# config can then hot-reload without overriding intentional .zshrc customizations.
typeset -gi AICOACH_COMPLETION_KEY_USER_SET=${+AICOACH_COMPLETION_KEY}
typeset -g AICOACH_COMPLETION_KEY_USER_VALUE=${AICOACH_COMPLETION_KEY-}
typeset -gi AICOACH_CHAT_KEY_USER_SET=${+AICOACH_CHAT_KEY}
typeset -g AICOACH_CHAT_KEY_USER_VALUE=${AICOACH_CHAT_KEY-}
typeset -gi AICOACH_RISK_LENS_KEY_USER_SET=${+AICOACH_RISK_LENS_KEY}
typeset -g AICOACH_RISK_LENS_KEY_USER_VALUE=${AICOACH_RISK_LENS_KEY-}
typeset -gi AICOACH_TOGGLE_KEY_USER_SET=${+AICOACH_TOGGLE_KEY}
typeset -g AICOACH_TOGGLE_KEY_USER_VALUE=${AICOACH_TOGGLE_KEY-}
typeset -gi AICOACH_LANGUAGE_USER_SET=${+AICOACH_LANGUAGE}
typeset -g AICOACH_LANGUAGE_USER_VALUE=${AICOACH_LANGUAGE-}
typeset -gi AICOACH_SAFETY_ENABLED_USER_SET=${+AICOACH_SAFETY_ENABLED}
typeset -g AICOACH_SAFETY_ENABLED_USER_VALUE=${AICOACH_SAFETY_ENABLED-}
typeset -gi AICOACH_INLINE_HINT_USER_SET=${+AICOACH_INLINE_HINT}
typeset -g AICOACH_INLINE_HINT_USER_VALUE=${AICOACH_INLINE_HINT-}

zmodload zsh/net/socket 2>/dev/null || return 0
zmodload zsh/system 2>/dev/null || return 0
zmodload zsh/zselect 2>/dev/null || true
zmodload zsh/datetime 2>/dev/null || true
autoload -Uz add-zsh-hook

typeset -g AICOACH_HOME=${AICOACH_HOME:-$HOME/.aicoach}
typeset -g AICOACH_SOCKET=${AICOACH_SOCKET:-$AICOACH_HOME/run/aicoach.sock}
typeset -g AICOACH_SESSION_ID=${AICOACH_SESSION_ID:-${$(command uuidgen 2>/dev/null):l}}
[[ -n $AICOACH_SESSION_ID ]] || typeset -g AICOACH_SESSION_ID="00000000-0000-4000-8000-$(printf '%012x' $(( ($$ << 16) ^ RANDOM )))"
typeset -g AICOACH_FD=""
typeset -g AICOACH_READ_BUFFER=""
typeset -g AICOACH_COMMAND=""
typeset -g AICOACH_COMMAND_ID=""
typeset -gF AICOACH_COMMAND_STARTED=0
typeset -g AICOACH_COMPLETION_ID=""
typeset -g AICOACH_COMPLETION_SNAPSHOT=""
typeset -g AICOACH_RISK_LENS_ID=""
typeset -g AICOACH_RISK_LENS_SNAPSHOT=""
typeset -g AICOACH_CHAT_ID=""
typeset -g AICOACH_CHAT_STREAM_CONTENT=""
typeset -g AICOACH_CHAT_STREAM_PENDING=""
typeset -gi AICOACH_CHAT_STREAM_STARTED=0
typeset -g AICOACH_CHAT_POSTDISPLAY_SAVED=""
typeset -g AICOACH_CHAT_POSTDISPLAY_DESIRED=""
typeset -g AICOACH_CHAT_MESSAGE_DESIRED=""
typeset -gi AICOACH_CHAT_POSTDISPLAY_ACTIVE=0
typeset -g AICOACH_LAST_CONNECT=0
typeset -g AICOACH_LAST_START=0
typeset -gi AICOACH_REQUEST_SEQ=0
typeset -ga AICOACH_PENDING_INSERTS
typeset -gi AICOACH_DEFER_INSERT=0
typeset -g AICOACH_PENDING_INSERT_RISK=""
typeset -g AICOACH_PENDING_INSERT_COVERAGE=""
typeset -g AICOACH_PENDING_INSERT_RULES=""
typeset -g AICOACH_PENDING_COMPLETION_OPERATION=""
typeset -g AICOACH_PENDING_COMPLETION_COMMAND=""
typeset -g AICOACH_PENDING_COMPLETION_CURSOR=""
typeset -g AICOACH_PENDING_COMPLETION_DESCRIPTION=""
typeset -g AICOACH_LAST_DANGER_BUFFER=""

typeset -g AICOACH_SETTINGS_FILE=${AICOACH_SETTINGS_FILE:-$HOME/.config/aicoach/keybindings.zsh}
typeset -g AICOACH_SETTINGS_VERSION_FILE=${AICOACH_SETTINGS_VERSION_FILE:-$HOME/.config/aicoach/keybindings.version}

_aicoach_apply_config_settings() {
  if (( AICOACH_COMPLETION_KEY_USER_SET )); then
    typeset -g AICOACH_COMPLETION_KEY=$AICOACH_COMPLETION_KEY_USER_VALUE
  else
    typeset -g AICOACH_COMPLETION_KEY=${AICOACH_CONFIG_COMPLETION_KEY:-$'\e\t'}
  fi
  if (( AICOACH_CHAT_KEY_USER_SET )); then
    typeset -g AICOACH_CHAT_KEY=$AICOACH_CHAT_KEY_USER_VALUE
  else
    typeset -g AICOACH_CHAT_KEY=${AICOACH_CONFIG_CHAT_KEY:-$'\e/'}
  fi
  if (( AICOACH_RISK_LENS_KEY_USER_SET )); then
    typeset -g AICOACH_RISK_LENS_KEY=$AICOACH_RISK_LENS_KEY_USER_VALUE
  else
    typeset -g AICOACH_RISK_LENS_KEY=${AICOACH_CONFIG_RISK_LENS_KEY:-$'\er'}
  fi
  if (( AICOACH_TOGGLE_KEY_USER_SET )); then
    typeset -g AICOACH_TOGGLE_KEY=$AICOACH_TOGGLE_KEY_USER_VALUE
  else
    typeset -g AICOACH_TOGGLE_KEY=${AICOACH_CONFIG_TOGGLE_KEY:-$'\e '}
  fi
  if (( AICOACH_LANGUAGE_USER_SET )); then
    typeset -g AICOACH_LANGUAGE=$AICOACH_LANGUAGE_USER_VALUE
  else
    typeset -g AICOACH_LANGUAGE=${AICOACH_CONFIG_LANGUAGE:-en-US}
  fi
  if (( AICOACH_SAFETY_ENABLED_USER_SET )); then
    typeset -gi AICOACH_SAFETY_ENABLED=$AICOACH_SAFETY_ENABLED_USER_VALUE
  else
    typeset -gi AICOACH_SAFETY_ENABLED=${AICOACH_CONFIG_SAFETY_ENABLED:-1}
  fi
  if (( AICOACH_INLINE_HINT_USER_SET )); then
    typeset -gi AICOACH_INLINE_HINT=$AICOACH_INLINE_HINT_USER_VALUE
  else
    typeset -gi AICOACH_INLINE_HINT=${AICOACH_CONFIG_INLINE_HINT:-1}
  fi
}

# Generated by `aicoach install`, `config validate`, or onboarding.
typeset -g AICOACH_CONFIG_COMPLETION_KEY=$'\e\t'
typeset -g AICOACH_CONFIG_CHAT_KEY=$'\e/'
typeset -g AICOACH_CONFIG_RISK_LENS_KEY=$'\er'
typeset -g AICOACH_CONFIG_TOGGLE_KEY=$'\e '
typeset -g AICOACH_CONFIG_LANGUAGE=en-US
typeset -gi AICOACH_CONFIG_SAFETY_ENABLED=1
typeset -gi AICOACH_CONFIG_INLINE_HINT=1
[[ -r $AICOACH_SETTINGS_FILE ]] && source $AICOACH_SETTINGS_FILE
_aicoach_apply_config_settings
typeset -g AICOACH_SETTINGS_VERSION=""
[[ -r $AICOACH_SETTINGS_VERSION_FILE ]] && typeset -g AICOACH_SETTINGS_VERSION=$(<$AICOACH_SETTINGS_VERSION_FILE)

# When Terminal.app's "Use Option as Meta key" setting is disabled, macOS
# emits the native Option glyphs instead of an Escape-prefixed sequence.
# Accept those spellings as fallbacks so the default shortcuts work without a
# terminal-profile change. Option+Tab has no safe distinct non-Meta sequence.
typeset -g AICOACH_CHAT_NATIVE_KEY=${AICOACH_CHAT_NATIVE_KEY:-$'÷'}
typeset -g AICOACH_RISK_LENS_NATIVE_KEY=${AICOACH_RISK_LENS_NATIVE_KEY:-$'®'}
typeset -g AICOACH_TOGGLE_NATIVE_KEY=${AICOACH_TOGGLE_NATIVE_KEY:-$'\u00a0'}

_aicoach_text() {
  local key=$1
  if [[ $AICOACH_LANGUAGE == zh-CN ]]; then
    case $key in
      streaming) REPLY='回答中…' ;;
      thinking) REPLY='正在思考…' ;;
      rejected_completion) REPLY='已拒绝包含终端控制字符或换行的 AI 补全。' ;;
      suggested_command) REPLY='建议命令' ;;
      empty_answer) REPLY='回答完成，但没有收到可显示的内容。' ;;
      rejected_command) REPLY='已拒绝包含终端控制字符或换行的命令。' ;;
      unavailable) REPLY='AI 服务暂时不可用' ;;
      critical_danger) REPLY='CRITICAL：该命令可能造成不可恢复的数据或系统损坏。' ;;
      recursive_delete) REPLY='HIGH：该命令会递归强制删除文件，请确认目标、备份和回滚方式。' ;;
      destructive) REPLY='HIGH：该命令具有破坏性，请确认目标、备份和回滚方式。' ;;
      generating_completion) REPLY='正在生成补全…（继续输入会自动丢弃旧结果）' ;;
      daemon_stopped) REPLY='后台服务未运行；请执行 aicoach start' ;;
      question_first) REPLY='请先在当前输入行写下问题，再按 Option+/' ;;
      lens_empty) REPLY='请先输入要检查的命令，再按 Option+R' ;;
      inspecting_locally) REPLY='正在本地分析命令影响…' ;;
      toggling) REPLY='正在切换 Coach 窗口…' ;;
      executable_missing) REPLY='找不到 aicoach 可执行文件' ;;
      *) REPLY=$key ;;
    esac
  else
    case $key in
      streaming) REPLY='Streaming…' ;;
      thinking) REPLY='Thinking…' ;;
      rejected_completion) REPLY='Rejected an AI completion containing terminal control characters or newlines.' ;;
      suggested_command) REPLY='Suggested command' ;;
      empty_answer) REPLY='The response completed without displayable content.' ;;
      rejected_command) REPLY='Rejected a command containing terminal control characters or newlines.' ;;
      unavailable) REPLY='AI service is temporarily unavailable' ;;
      critical_danger) REPLY='CRITICAL: this command may cause irreversible data or system damage.' ;;
      recursive_delete) REPLY='HIGH: this command recursively force-deletes files; verify the target, backup, and recovery plan.' ;;
      destructive) REPLY='HIGH: this command is destructive; verify the target, backup, and recovery plan.' ;;
      generating_completion) REPLY='Generating completion… (continued typing discards the stale result)' ;;
      daemon_stopped) REPLY='Daemon is not running; run: aicoach start' ;;
      question_first) REPLY='Type a question in the current input line, then press Option+/' ;;
      lens_empty) REPLY='Type a command to inspect, then press Option+R' ;;
      inspecting_locally) REPLY='Inspecting command impact locally…' ;;
      toggling) REPLY='Toggling the Coach window…' ;;
      executable_missing) REPLY='The aicoach executable was not found' ;;
      *) REPLY=$key ;;
    esac
  fi
}

_aicoach_now_ms() {
  local now=${EPOCHREALTIME:-0}
  REPLY=${${now/./}%%[!0-9]*}
  while (( ${#REPLY} < 13 )); do REPLY+="0"; done
  REPLY=${REPLY[1,13]}
}

_aicoach_request_id() {
  (( ++AICOACH_REQUEST_SEQ ))
  local a=$(( (RANDOM << 16) ^ RANDOM ^ AICOACH_REQUEST_SEQ ))
  local b=$(( RANDOM ^ AICOACH_REQUEST_SEQ ))
  local c=$(( RANDOM & 0x0fff ))
  local d=$(( (RANDOM & 0x3fff) | 0x8000 ))
  local e=$(( (RANDOM << 16) ^ RANDOM ^ $$ ))
  printf -v REPLY '%08x-%04x-4%03x-%04x-%04x%08x' $(( a & 0xffffffff )) $(( b & 0xffff )) $c $d $(( RANDOM & 0xffff )) $(( e & 0xffffffff ))
}

# The line protocol only reserves percent, tab, CR, and LF. Keeping all other
# UTF-8 intact avoids locale-dependent shell escaping and remains human-debuggable.
_aicoach_encode() {
  local value=$1
  value=${value//\%/%25}
  value=${value//$'\t'/%09}
  value=${value//$'\r'/%0D}
  value=${value//$'\n'/%0A}
  REPLY=$value
}

_aicoach_decode() {
  local value=$1
  value=${value//\%0A/$'\n'}
  value=${value//\%0a/$'\n'}
  value=${value//\%0D/$'\r'}
  value=${value//\%0d/$'\r'}
  value=${value//\%09/$'\t'}
  value=${value//\%25/\%}
  REPLY=$value
}

# Capture only low-risk metadata that materially changes shell behavior. The
# list is intentionally fixed here and enforced again by the daemon; never
# enumerate the process environment because it commonly contains credentials.
_aicoach_environment_snapshot() {
  local -a keys=(LANG LC_ALL LC_CTYPE TERM COLORTERM VIRTUAL_ENV CONDA_DEFAULT_ENV)
  local key value snapshot=""
  for key in $keys; do
    if (( ${+parameters[$key]} )); then
      value=${(P)key}
      # Newlines delimit assignments in the payload. Replace controls so one
      # value cannot forge another allowlisted assignment.
      value=${value//$'\n'/ }
      value=${value//$'\r'/ }
      value=${value//[[:cntrl:]]/}
      value=${value[1,4096]}
      snapshot+="${key}=${value}"$'\n'
    fi
  done
  REPLY=$snapshot
}

# Provider output and captured terminal text are untrusted. Strip every C0/C1
# display control before passing text to `print` or `zle -M`; otherwise an
# OpenAI-compatible endpoint could inject OSC/CSI sequences into the terminal.
_aicoach_safe_display() {
  local value=$1
  value=${value//$'\e'/}
  value=${value//$'\r'/ }
  value=${value//$'\n'/ }
  value=${value//$'\t'/ }
  value=${value//[[:cntrl:]]/}
  REPLY=$value
}

_aicoach_safe_multiline_display() {
  local value=$1 output="" character
  # Preserve only LF as a structural control and turn tabs into two spaces.
  # Every other C0/C1 character, including ESC, is discarded.
  for character in ${(s::)value}; do
    case $character in
      $'\n') output+=$'\n' ;;
      $'\t') output+='  ' ;;
      $'\r') ;;
      [[:cntrl:]]) ;;
      *) output+=$character ;;
    esac
  done
  REPLY=$output
}

_aicoach_safe_buffer() {
  local value=$1
  [[ $value != *$'\e'* && $value != *$'\r'* && $value != *$'\n'* && $value != *[[:cntrl:]]* ]]
}

_aicoach_close() {
  if [[ -n $AICOACH_FD ]]; then
    zle -F "$AICOACH_FD" 2>/dev/null || true
    exec {AICOACH_FD}>&- 2>/dev/null || true
  fi
  typeset -g AICOACH_FD=""
}

_aicoach_maybe_start() {
  (( $+commands[aicoach] )) || return 0
  _aicoach_now_ms
  local now=$REPLY
  # A failed launch must not disable recovery for the lifetime of the shell.
  # The cooldown still prevents redisplay hooks from spawning a process storm.
  (( now - AICOACH_LAST_START < 5000 )) && return 0
  typeset -g AICOACH_LAST_START=$now
  command aicoach start >/dev/null 2>&1 &!
}

_aicoach_connect() {
  [[ -n $AICOACH_FD ]] && return 0
  if [[ ! -S $AICOACH_SOCKET ]]; then
    _aicoach_maybe_start
    return 1
  fi

  _aicoach_now_ms
  local now=$REPLY
  # Avoid doing filesystem/socket work on every redisplay while the daemon is down.
  (( now - AICOACH_LAST_CONNECT < 1000 )) && return 1
  typeset -g AICOACH_LAST_CONNECT=$now

  if ! zsocket "$AICOACH_SOCKET" 2>/dev/null; then
    _aicoach_maybe_start
    return 1
  fi
  typeset -g AICOACH_FD=$REPLY

  local tty_name=${TTY:-$(tty 2>/dev/null)} encoded_cwd encoded_terminal encoded_environment
  _aicoach_encode "$PWD"; encoded_cwd=$REPLY
  _aicoach_encode "${TERM_PROGRAM:-unknown}"; encoded_terminal=$REPLY
  _aicoach_environment_snapshot
  _aicoach_encode "$REPLY"; encoded_environment=$REPLY
  if ! syswrite -o "$AICOACH_FD" -- $'ZSH\tREGISTER\t'"$AICOACH_SESSION_ID"$'\t'"$tty_name"$'\t'"$$"$'\t'"$encoded_cwd"$'\t'"$encoded_terminal"$'\t'"$encoded_environment"$'\n' 2>/dev/null; then
    _aicoach_close
    return 1
  fi
  # `_aicoach_connect` can first succeed from a completion/chat widget after
  # line-init already ran while the daemon was still starting.
  zle -F "$AICOACH_FD" _aicoach_socket_ready 2>/dev/null || true
  typeset -g AICOACH_LAST_START=0
  return 0
}

_aicoach_send() {
  local line=$1
  _aicoach_connect || return 1
  if ! syswrite -o "$AICOACH_FD" -- "${line}"$'\n' 2>/dev/null; then
    _aicoach_close
    return 1
  fi
}

_aicoach_color_for() {
  case ${1:l} in
    critical|error|high) REPLY=$'\e[1;31m' ;;
    warning|medium)      REPLY=$'\e[1;33m' ;;
    success|low)         REPLY=$'\e[1;32m' ;;
    *)                   REPLY=$'\e[1;34m' ;;
  esac
}

_aicoach_insert_status() {
  local risk=${1:l} coverage=${2:l} rules=${3:l} rating qualifier=""
  case $risk in
    low|medium|high|critical|unrated) ;;
    *) risk=unrated ;;
  esac
  case $coverage in
    recognized|partial|unknown) ;;
    *) coverage=unknown ;;
  esac

  if [[ $AICOACH_LANGUAGE == zh-CN ]]; then
    case $risk in
      low) rating='低风险' ;;
      medium) rating='中风险' ;;
      high) rating='高风险' ;;
      critical) rating='严重风险' ;;
      *) rating='未评级' ;;
    esac
    case $coverage in
      partial) qualifier=' · 部分识别' ;;
      unknown) qualifier=' · 未识别命令' ;;
    esac
    [[ $rules == false ]] && qualifier+=' · 破坏性规则已关闭'
    zle -M "[AI Coach] 仅插入 · ${rating}${qualifier} · 尚未执行；检查后请自行按 Enter"
  else
    rating=${risk:u}
    case $coverage in
      partial) qualifier=' · partial coverage' ;;
      unknown) qualifier=' · unknown command' ;;
    esac
    [[ $rules == false ]] && qualifier+=' · destructive rules off'
    zle -M "[AI Coach] Insert only · ${rating}${qualifier} · not executed; review, then press Enter"
  fi
}

_aicoach_notice() {
  local severity=$1 message=$2 suggested=${3:-}
  _aicoach_safe_multiline_display "$message"; message=$REPLY
  _aicoach_safe_display "$suggested"; suggested=$REPLY
  _aicoach_color_for "$severity"; local color=$REPLY reset=$'\e[0m'
  zle -I 2>/dev/null || true
  print -r -- "${color}[AI Coach]:${reset} ${message}"
  [[ -n $suggested ]] && print -r -- $'\e[90m  → '"${suggested}"$'\e[0m'
  zle reset-prompt 2>/dev/null || true
  zle redisplay 2>/dev/null || true
}

_aicoach_chat_stream_reset() {
  if (( AICOACH_CHAT_POSTDISPLAY_ACTIVE )); then
    typeset -g AICOACH_CHAT_POSTDISPLAY_DESIRED=$AICOACH_CHAT_POSTDISPLAY_SAVED
    typeset -g AICOACH_CHAT_MESSAGE_DESIRED=""
    zle aicoach-apply-chat-display 2>/dev/null || true
  fi
  typeset -g AICOACH_CHAT_ID=""
  typeset -g AICOACH_CHAT_STREAM_CONTENT=""
  typeset -g AICOACH_CHAT_STREAM_PENDING=""
  typeset -gi AICOACH_CHAT_STREAM_STARTED=0
  typeset -g AICOACH_CHAT_POSTDISPLAY_SAVED=""
  typeset -g AICOACH_CHAT_POSTDISPLAY_DESIRED=""
  typeset -g AICOACH_CHAT_MESSAGE_DESIRED=""
  typeset -gi AICOACH_CHAT_POSTDISPLAY_ACTIVE=0
}

_aicoach_chat_stream_status() {
  local message=$1
  # Keep the persistent preview short enough for double-width CJK text beside
  # a typical prompt; the full accumulated answer is rendered below by ZLE.
  _aicoach_safe_display "$message"
  local preview=$REPLY preview_chars=12
  typeset -g AICOACH_CHAT_MESSAGE_DESIRED=$message
  if (( AICOACH_CHAT_STREAM_STARTED && ${#preview} > preview_chars )); then
    local preview_start=$(( ${#preview} - preview_chars + 1 ))
    _aicoach_text streaming
    preview="${REPLY}${preview[$preview_start,-1]}"
  fi
  if [[ -n $AICOACH_CHAT_POSTDISPLAY_SAVED ]]; then
    typeset -g AICOACH_CHAT_POSTDISPLAY_DESIRED="${AICOACH_CHAT_POSTDISPLAY_SAVED}  [AI Coach] ${preview}"
  else
    typeset -g AICOACH_CHAT_POSTDISPLAY_DESIRED="[AI Coach] ${preview}"
  fi
  # Socket callbacks are transactional ZLE contexts: assigning POSTDISPLAY
  # directly there is rolled back when the callback returns. Invoke a named
  # widget so the display change is committed, just like BUFFER insertions.
  zle aicoach-apply-chat-display 2>/dev/null || true
}

_aicoach_chat_stream_begin() {
  typeset -g AICOACH_CHAT_POSTDISPLAY_SAVED=${POSTDISPLAY:-}
  typeset -gi AICOACH_CHAT_POSTDISPLAY_ACTIVE=1
  _aicoach_text thinking
  _aicoach_chat_stream_status "$REPLY"
}

_aicoach_apply_chat_display_widget() {
  POSTDISPLAY=$AICOACH_CHAT_POSTDISPLAY_DESIRED
  zle redisplay 2>/dev/null || true
  if (( AICOACH_CHAT_STREAM_STARTED )) && [[ -n $AICOACH_CHAT_MESSAGE_DESIRED ]]; then
    zle -M "[AI Coach] ${AICOACH_CHAT_MESSAGE_DESIRED}"
  else
    zle -M ""
  fi
}

_aicoach_chat_stream_render() {
  local force=${1:-0}
  [[ -n $AICOACH_CHAT_STREAM_PENDING ]] || return 0
  if (( force || ! AICOACH_CHAT_STREAM_STARTED || ${#AICOACH_CHAT_STREAM_PENDING} >= 24 )) ||
      [[ $AICOACH_CHAT_STREAM_PENDING == *[。！？.!?] ]]; then
    typeset -gi AICOACH_CHAT_STREAM_STARTED=1
    typeset -g AICOACH_CHAT_STREAM_PENDING=""
    _aicoach_chat_stream_status "$AICOACH_CHAT_STREAM_CONTENT"
  fi
}

_aicoach_handle_line() {
  local line=$1 normalized
  normalized=${line//$'\t'/$'\n'}
  local -a fields=("${(@f)normalized}")
  (( ${#fields} >= 1 )) || return 0

  local kind=${fields[1]} session=${fields[2]:-}
  [[ -z $session || $session == $AICOACH_SESSION_ID ]] || return 0

  case $kind in
    HINT)
      (( AICOACH_INLINE_HINT )) || return 0
      local severity=${fields[4]:-info} message suggested
      _aicoach_decode "${fields[5]:-}"; message=$REPLY
      _aicoach_decode "${fields[6]:-}"; suggested=$REPLY
      _aicoach_notice "$severity" "$message" "$suggested"
      ;;
    COMPLETE)
      local request_id=${fields[3]:-} operation=${fields[4]:-suggest} cursor=${fields[5]:--1}
      local command description
      _aicoach_decode "${fields[6]:-}"; command=$REPLY
      _aicoach_decode "${fields[7]:-}"; description=$REPLY
      [[ $request_id == $AICOACH_COMPLETION_ID ]] || return 0
      [[ $BUFFER == $AICOACH_COMPLETION_SNAPSHOT ]] || return 0
      if ! _aicoach_safe_buffer "$command"; then
        _aicoach_text rejected_completion
        _aicoach_notice error "$REPLY"
        return 0
      fi
      if _aicoach_local_danger "$command"; then
        _aicoach_notice critical "$REPLY" "$command"
        return 0
      fi
      _aicoach_safe_display "$description"; description=$REPLY
      case $operation in
        replace|insert)
          typeset -g AICOACH_PENDING_COMPLETION_OPERATION=$operation
          typeset -g AICOACH_PENDING_COMPLETION_COMMAND=$command
          typeset -g AICOACH_PENDING_COMPLETION_CURSOR=$cursor
          typeset -g AICOACH_PENDING_COMPLETION_DESCRIPTION=$description
          zle aicoach-apply-completion
          ;;
        *)
          if [[ -z $description ]]; then _aicoach_text suggested_command; description=$REPLY; fi
          _aicoach_notice info "$description" "$command"
          ;;
      esac
      ;;
    LENS)
      local request_id=${fields[3]:-} severity=${fields[4]:-unrated} message
      local snapshot=$AICOACH_RISK_LENS_SNAPSHOT
      _aicoach_decode "${fields[5]:-}"; message=$REPLY
      [[ $request_id == $AICOACH_RISK_LENS_ID ]] || return 0
      typeset -g AICOACH_RISK_LENS_ID=""
      typeset -g AICOACH_RISK_LENS_SNAPSHOT=""
      [[ $BUFFER == $snapshot ]] || return 0
      [[ $severity == unrated ]] && severity=warning
      _aicoach_notice "$severity" "$message"
      ;;
    ANSWER)
      local request_id=${fields[3]:-} message
      [[ -z $AICOACH_CHAT_ID || $request_id == $AICOACH_CHAT_ID ]] || return 0
      _aicoach_decode "${fields[4]:-}"; message=$REPLY
      _aicoach_chat_stream_reset
      _aicoach_notice info "$message"
      ;;
    ANSWER_DELTA)
      local request_id=${fields[3]:-} delta
      [[ -n $AICOACH_CHAT_ID && $request_id == $AICOACH_CHAT_ID ]] || return 0
      _aicoach_decode "${fields[4]:-}"; delta=$REPLY
      _aicoach_safe_multiline_display "$delta"; delta=$REPLY
      AICOACH_CHAT_STREAM_CONTENT="${AICOACH_CHAT_STREAM_CONTENT}${delta}"
      AICOACH_CHAT_STREAM_PENDING="${AICOACH_CHAT_STREAM_PENDING}${delta}"
      _aicoach_chat_stream_render 0
      ;;
    ANSWER_DONE)
      local request_id=${fields[3]:-} answer=$AICOACH_CHAT_STREAM_CONTENT
      [[ -n $AICOACH_CHAT_ID && $request_id == $AICOACH_CHAT_ID ]] || return 0
      _aicoach_chat_stream_reset
      if [[ -n $answer ]]; then
        _aicoach_notice info "$answer"
      else
        _aicoach_text empty_answer
        _aicoach_notice warning "$REPLY"
      fi
      ;;
    INSERT)
      local command risk=${fields[4]:-unrated} encoded_coverage=${fields[5]:-} rules=${fields[6]:-}
      local coverage=${encoded_coverage:-unknown}
      _aicoach_decode "${fields[3]:-}"; command=$REPLY
      if ! _aicoach_safe_buffer "$command"; then
        _aicoach_text rejected_command
        _aicoach_notice error "$REPLY"
        return 0
      fi
      if _aicoach_local_danger "$command"; then
        case $risk:$REPLY in
          critical:*) ;;
          *:CRITICAL:*|*:CRITICAL：*) risk=critical ;;
          *) risk=high ;;
        esac
        # Preserve a daemon-reported partial classification for compound
        # commands. Only old three-field INSERT frames need fallback coverage.
        [[ -z $encoded_coverage ]] && coverage=recognized
      fi
      AICOACH_PENDING_INSERTS+=("$command")
      typeset -g AICOACH_PENDING_INSERT_RISK=$risk
      typeset -g AICOACH_PENDING_INSERT_COVERAGE=$coverage
      typeset -g AICOACH_PENDING_INSERT_RULES=$rules
      (( AICOACH_DEFER_INSERT )) || zle aicoach-apply-pending
      ;;
    ERROR)
      local request_id=${fields[3]:-} message partial=""
      _aicoach_decode "${fields[5]:-${fields[4]:-}}"; message=$REPLY
      if [[ -n $AICOACH_CHAT_ID && $request_id == $AICOACH_CHAT_ID ]]; then
        partial=$AICOACH_CHAT_STREAM_CONTENT
        _aicoach_chat_stream_reset
      fi
      if [[ -n $partial ]]; then
        _aicoach_text unavailable; local unavailable=$REPLY
        if [[ $AICOACH_LANGUAGE == zh-CN ]]; then
          _aicoach_notice warning "${partial} …（回答中断：${message:-$unavailable}）"
        else
          _aicoach_notice warning "${partial} … (response interrupted: ${message:-$unavailable})"
        fi
      else
        _aicoach_text unavailable
        _aicoach_notice error "${message:-$REPLY}"
      fi
      ;;
  esac
}

_aicoach_socket_ready() {
  local fd=$1 chunk
  if ! sysread -i "$fd" -s 65536 chunk 2>/dev/null; then
    _aicoach_close
    return 0
  fi
  AICOACH_READ_BUFFER+=$chunk
  while [[ $AICOACH_READ_BUFFER == *$'\n'* ]]; do
    local line=${AICOACH_READ_BUFFER%%$'\n'*}
    typeset -g AICOACH_READ_BUFFER=${AICOACH_READ_BUFFER#*$'\n'}
    [[ -n $line ]] && _aicoach_handle_line "$line"
  done
}

_aicoach_preexec() {
  typeset -g AICOACH_COMMAND=$1
  _aicoach_request_id; typeset -g AICOACH_COMMAND_ID=$REPLY
  _aicoach_now_ms; typeset -gF AICOACH_COMMAND_STARTED=$REPLY
  local encoded_cwd encoded_command
  _aicoach_encode "$PWD"; encoded_cwd=$REPLY
  _aicoach_encode "$AICOACH_COMMAND"; encoded_command=$REPLY
  _aicoach_send $'ZSH\tPREEXEC\t'"$AICOACH_SESSION_ID"$'\t'"$AICOACH_COMMAND_ID"$'\t'"$encoded_cwd"$'\t'"$encoded_command"$'\t'"${AICOACH_COMMAND_STARTED%.*}" || true
}

_aicoach_precmd() {
  local exit_code=$?
  _aicoach_refresh_settings
  # A directly launched `aicoach-ui` blocks ZLE. Drain any queued INSERT frame
  # before the next line editor starts, then apply it from line-init below.
  if [[ -n $AICOACH_FD ]] && (( $+builtins[zselect] )) && \
      zselect -t 0 -r "$AICOACH_FD" 2>/dev/null; then
    typeset -g AICOACH_DEFER_INSERT=1
    _aicoach_socket_ready "$AICOACH_FD"
    typeset -g AICOACH_DEFER_INSERT=0
  fi
  [[ -n $AICOACH_COMMAND_ID ]] || { _aicoach_connect || true; return 0; }
  _aicoach_now_ms
  local ended=$REPLY
  local duration=$(( ended - AICOACH_COMMAND_STARTED )) encoded_cwd encoded_environment
  (( duration < 0 )) && duration=0
  _aicoach_encode "$PWD"; encoded_cwd=$REPLY
  _aicoach_environment_snapshot
  _aicoach_encode "$REPLY"; encoded_environment=$REPLY
  _aicoach_send $'ZSH\tFINISH\t'"$AICOACH_SESSION_ID"$'\t'"$AICOACH_COMMAND_ID"$'\t'"$exit_code"$'\t'"$encoded_cwd"$'\t'"${duration%.*}"$'\t'"$encoded_environment" || true
  typeset -g AICOACH_COMMAND_ID=""
}

_aicoach_apply_pending_inserts() {
  if (( ${#AICOACH_PENDING_INSERTS} )); then
    local risk=$AICOACH_PENDING_INSERT_RISK coverage=$AICOACH_PENDING_INSERT_COVERAGE rules=$AICOACH_PENDING_INSERT_RULES
    BUFFER=${AICOACH_PENDING_INSERTS[-1]}
    CURSOR=${#BUFFER}
    AICOACH_PENDING_INSERTS=()
    typeset -g AICOACH_PENDING_INSERT_RISK=""
    typeset -g AICOACH_PENDING_INSERT_COVERAGE=""
    typeset -g AICOACH_PENDING_INSERT_RULES=""
    zle redisplay
    _aicoach_insert_status "$risk" "$coverage" "$rules"
  fi
}

_aicoach_apply_pending_widget() {
  _aicoach_apply_pending_inserts
}

_aicoach_apply_completion_widget() {
  local operation=$AICOACH_PENDING_COMPLETION_OPERATION
  local command=$AICOACH_PENDING_COMPLETION_COMMAND
  local cursor=$AICOACH_PENDING_COMPLETION_CURSOR
  local description=$AICOACH_PENDING_COMPLETION_DESCRIPTION
  typeset -g AICOACH_PENDING_COMPLETION_OPERATION=""
  typeset -g AICOACH_PENDING_COMPLETION_COMMAND=""
  typeset -g AICOACH_PENDING_COMPLETION_CURSOR=""
  typeset -g AICOACH_PENDING_COMPLETION_DESCRIPTION=""
  case $operation in
    replace)
      BUFFER=$command
      if [[ $cursor == <-> ]] && (( cursor >= 0 && cursor <= ${#BUFFER} )); then
        CURSOR=$cursor
      else
        CURSOR=${#BUFFER}
      fi
      ;;
    insert)
      local left=${BUFFER[1,CURSOR]} right=${BUFFER[CURSOR+1,-1]}
      BUFFER="${left}${command}${right}"
      (( CURSOR += ${#command} ))
      ;;
    *) return 0 ;;
  esac
  zle redisplay
  [[ -n $description ]] && zle -M "[AI Coach] $description"
}

_aicoach_line_init() {
  if _aicoach_connect; then
    zle -F "$AICOACH_FD" _aicoach_socket_ready 2>/dev/null || true
    local tty_name=${TTY:-unknown}
    _aicoach_send $'ZSH\tFOCUS\t'"$AICOACH_SESSION_ID"$'\t'"$tty_name" || true
  fi
  _aicoach_apply_pending_inserts
}

_aicoach_local_danger() {
  (( AICOACH_SAFETY_ENABLED )) || return 1
  local value=${1:l}
  local boundary='(^|[;&|()][[:space:]]*)'
  local rm_prefix="${boundary}(sudo[[:space:]]+)?rm[[:space:]]+"
  local rm_forced="${rm_prefix}([^;&|]*[[:space:]])?(-[^;&|[:space:]]*r[^;&|[:space:]]*f|-([^;&|[:space:]]*[[:space:]])*r([^;&|[:space:]]*[[:space:]])*-f|-([^;&|[:space:]]*[[:space:]])*f([^;&|[:space:]]*[[:space:]])*-r)([[:space:]]|$)"
  if [[ $value =~ $rm_forced ]]; then
    local destructive_root="${rm_prefix}[^;&|]*[[:space:]](/|~|\\\$home)([[:space:];&|]|$)"
    if [[ $value =~ $destructive_root ]]; then
      _aicoach_text critical_danger
    else
      _aicoach_text recursive_delete
    fi
    return 0
  fi
  local critical_command="${boundary}((sudo[[:space:]]+)?mkfs([^;&|[:space:]]*)?|diskutil[[:space:]]+erasedisk)([[:space:]]|$)"
  local critical_sql="${boundary}((mysql|mariadb|psql|sqlite3)[^;&|]*[[:space:]])?drop[[:space:]]+database([[:space:];&|]|$)"
  if [[ $value =~ $critical_command || $value =~ $critical_sql ]]; then
    _aicoach_text critical_danger
    return 0
  fi
  local high_command="${boundary}(git[[:space:]]+(reset[[:space:]]+--hard|clean[[:space:]]+-[^;&|[:space:]]*f[^;&|[:space:]]*d)|chmod[[:space:]]+-r[[:space:]]+777|dd[[:space:]]+[^;&|]*if=|kill[[:space:]]+-9)([[:space:]]|$)"
  local high_sql="${boundary}((mysql|mariadb|psql|sqlite3)[^;&|]*[[:space:]])?drop[[:space:]]+table([[:space:];&|]|$)"
  if [[ $value =~ $high_command || $value =~ $high_sql ]]; then
    _aicoach_text destructive
    return 0
  fi
  return 1
}

_aicoach_complete_widget() {
  if _aicoach_local_danger "$BUFFER"; then
    _aicoach_notice critical "$REPLY"
    return 0
  fi
  [[ -n $AICOACH_COMPLETION_ID ]] && _aicoach_send $'ZSH\tCANCEL\t'"$AICOACH_SESSION_ID"$'\t'"$AICOACH_COMPLETION_ID" || true
  _aicoach_request_id; typeset -g AICOACH_COMPLETION_ID=$REPLY
  typeset -g AICOACH_COMPLETION_SNAPSHOT=$BUFFER
  local encoded_cwd encoded_buffer
  _aicoach_encode "$PWD"; encoded_cwd=$REPLY
  _aicoach_encode "$BUFFER"; encoded_buffer=$REPLY
  if _aicoach_send $'ZSH\tCOMPLETE\t'"$AICOACH_SESSION_ID"$'\t'"$AICOACH_COMPLETION_ID"$'\t'"$CURSOR"$'\t'"$encoded_cwd"$'\t'"$encoded_buffer"; then
    _aicoach_text generating_completion; zle -M "[AI Coach] $REPLY"
  else
    _aicoach_text daemon_stopped; zle -M "[AI Coach] $REPLY"
  fi
}

_aicoach_chat_widget() {
  local question=$BUFFER
  if [[ -z ${question//[[:space:]]/} ]]; then
    _aicoach_text question_first; zle -M "[AI Coach] $REPLY"
    return 0
  fi
  local previous_request=$AICOACH_CHAT_ID
  if [[ -n $previous_request ]]; then
    _aicoach_send $'ZSH\tCANCEL\t'"$AICOACH_SESSION_ID"$'\t'"$previous_request" >/dev/null 2>&1 || true
  fi
  _aicoach_chat_stream_reset
  _aicoach_request_id; typeset -g AICOACH_CHAT_ID=$REPLY
  local encoded_cwd encoded_question
  _aicoach_encode "$PWD"; encoded_cwd=$REPLY
  _aicoach_encode "$question"; encoded_question=$REPLY
  if _aicoach_send $'ZSH\tCHAT\t'"$AICOACH_SESSION_ID"$'\t'"$AICOACH_CHAT_ID"$'\t'"$encoded_cwd"$'\t\t'"$encoded_question"; then
    BUFFER=""
    CURSOR=0
    _aicoach_chat_stream_begin
  else
    _aicoach_chat_stream_reset
    _aicoach_text daemon_stopped; zle -M "[AI Coach] $REPLY"
  fi
}

_aicoach_risk_lens_widget() {
  if [[ -z ${BUFFER//[[:space:]]/} ]]; then
    _aicoach_text lens_empty; zle -M "[AI Coach] $REPLY"
    return 0
  fi
  _aicoach_request_id; typeset -g AICOACH_RISK_LENS_ID=$REPLY
  typeset -g AICOACH_RISK_LENS_SNAPSHOT=$BUFFER
  local encoded_cwd encoded_buffer
  _aicoach_encode "$PWD"; encoded_cwd=$REPLY
  _aicoach_encode "$BUFFER"; encoded_buffer=$REPLY
  if _aicoach_send $'ZSH\tLENS\t'"$AICOACH_SESSION_ID"$'\t'"$AICOACH_RISK_LENS_ID"$'\t'"$encoded_cwd"$'\t'"$encoded_buffer"; then
    _aicoach_text inspecting_locally; zle -M "[AI Coach] $REPLY"
  else
    typeset -g AICOACH_RISK_LENS_ID=""
    typeset -g AICOACH_RISK_LENS_SNAPSHOT=""
    _aicoach_text daemon_stopped; zle -M "[AI Coach] $REPLY"
  fi
}

_aicoach_toggle_widget() {
  if (( $+commands[aicoach] )); then
    command aicoach toggle --session "$AICOACH_SESSION_ID" --tty "${TTY:-unknown}" >/dev/null 2>&1 &!
    _aicoach_text toggling; zle -M "[AI Coach] $REPLY"
  else
    _aicoach_text executable_missing; zle -M "[AI Coach] $REPLY"
  fi
}

_aicoach_line_pre_redraw() {
  if [[ $BUFFER != $AICOACH_LAST_DANGER_BUFFER ]] && _aicoach_local_danger "$BUFFER"; then
    typeset -g AICOACH_LAST_DANGER_BUFFER=$BUFFER
    zle -M "[AI Coach] $REPLY"
  elif [[ $BUFFER != $AICOACH_LAST_DANGER_BUFFER ]]; then
    typeset -g AICOACH_LAST_DANGER_BUFFER=""
  fi
  # Typing after a completion request makes its response stale. Tell the daemon
  # early so provider capacity is immediately released.
  if [[ -n $AICOACH_COMPLETION_ID && $BUFFER != $AICOACH_COMPLETION_SNAPSHOT ]]; then
    _aicoach_send $'ZSH\tCANCEL\t'"$AICOACH_SESSION_ID"$'\t'"$AICOACH_COMPLETION_ID" || true
    typeset -g AICOACH_COMPLETION_ID=""
    typeset -g AICOACH_COMPLETION_SNAPSHOT=""
  fi
}

_aicoach_zshexit() {
  [[ -n $AICOACH_FD ]] && _aicoach_send $'ZSH\tDISCONNECT\t'"$AICOACH_SESSION_ID" || true
  _aicoach_close
}

zle -N aicoach-complete _aicoach_complete_widget
zle -N aicoach-chat _aicoach_chat_widget
zle -N aicoach-risk-lens _aicoach_risk_lens_widget
zle -N aicoach-toggle _aicoach_toggle_widget
zle -N aicoach-apply-pending _aicoach_apply_pending_widget
zle -N aicoach-apply-completion _aicoach_apply_completion_widget
zle -N aicoach-apply-chat-display _aicoach_apply_chat_display_widget

_aicoach_bind_widget() {
  local sequence=$1 widget=$2 keymap
  # Bind both common insert-mode maps. `main` may point at either one and can
  # change after this integration is sourced (for example by `bindkey -v`).
  for keymap in emacs viins; do
    bindkey -M "$keymap" "$sequence" "$widget" 2>/dev/null || true
  done
}

_aicoach_unbind_widget() {
  local sequence=$1 widget=$2 keymap current
  [[ -n $sequence ]] || return 0
  for keymap in emacs viins; do
    current=$(bindkey -M "$keymap" "$sequence" 2>/dev/null) || continue
    # Remove only a binding still owned by this integration. A plugin/user may
    # have deliberately replaced the same sequence since it was installed.
    if [[ $current == *" $widget" ]]; then
      bindkey -M "$keymap" -r "$sequence" 2>/dev/null || true
    fi
  done
}

_aicoach_bind_all_widgets() {
  _aicoach_bind_widget "$AICOACH_COMPLETION_KEY" aicoach-complete
  _aicoach_bind_widget "$AICOACH_CHAT_KEY" aicoach-chat
  [[ $AICOACH_CHAT_NATIVE_KEY == $AICOACH_CHAT_KEY ]] || \
    _aicoach_bind_widget "$AICOACH_CHAT_NATIVE_KEY" aicoach-chat
  _aicoach_bind_widget "$AICOACH_RISK_LENS_KEY" aicoach-risk-lens
  [[ $AICOACH_RISK_LENS_NATIVE_KEY == $AICOACH_RISK_LENS_KEY ]] || \
    _aicoach_bind_widget "$AICOACH_RISK_LENS_NATIVE_KEY" aicoach-risk-lens
  _aicoach_bind_widget "$AICOACH_TOGGLE_KEY" aicoach-toggle
  [[ $AICOACH_TOGGLE_NATIVE_KEY == $AICOACH_TOGGLE_KEY ]] || \
    _aicoach_bind_widget "$AICOACH_TOGGLE_NATIVE_KEY" aicoach-toggle
}

_aicoach_reload_settings() {
  local old_completion=$AICOACH_COMPLETION_KEY
  local old_chat=$AICOACH_CHAT_KEY
  local old_risk_lens=$AICOACH_RISK_LENS_KEY
  local old_toggle=$AICOACH_TOGGLE_KEY

  typeset -g AICOACH_CONFIG_COMPLETION_KEY=$'\e\t'
  typeset -g AICOACH_CONFIG_CHAT_KEY=$'\e/'
  typeset -g AICOACH_CONFIG_RISK_LENS_KEY=$'\er'
  typeset -g AICOACH_CONFIG_TOGGLE_KEY=$'\e '
  typeset -g AICOACH_CONFIG_LANGUAGE=en-US
  typeset -gi AICOACH_CONFIG_SAFETY_ENABLED=1
  typeset -gi AICOACH_CONFIG_INLINE_HINT=1
  [[ -r $AICOACH_SETTINGS_FILE ]] && source $AICOACH_SETTINGS_FILE
  _aicoach_apply_config_settings

  _aicoach_unbind_widget "$old_completion" aicoach-complete
  _aicoach_unbind_widget "$old_chat" aicoach-chat
  _aicoach_unbind_widget "$old_risk_lens" aicoach-risk-lens
  _aicoach_unbind_widget "$old_toggle" aicoach-toggle
  _aicoach_bind_all_widgets
  if [[ -r $AICOACH_SETTINGS_VERSION_FILE ]]; then
    typeset -g AICOACH_SETTINGS_VERSION=$(<$AICOACH_SETTINGS_VERSION_FILE)
  fi
}

_aicoach_refresh_settings() {
  [[ -r $AICOACH_SETTINGS_VERSION_FILE ]] || return 0
  local current_version=$(<$AICOACH_SETTINGS_VERSION_FILE)
  [[ $current_version == $AICOACH_SETTINGS_VERSION ]] && return 0
  _aicoach_reload_settings
}

_aicoach_bind_all_widgets

add-zsh-hook preexec _aicoach_preexec
add-zsh-hook precmd _aicoach_precmd
add-zsh-hook zshexit _aicoach_zshexit

autoload -Uz add-zle-hook-widget 2>/dev/null && {
  add-zle-hook-widget line-init _aicoach_line_init
  add-zle-hook-widget line-pre-redraw _aicoach_line_pre_redraw
}

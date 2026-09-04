#!/bin/zsh
set -euo pipefail

typeset -gr repo_root=${0:A:h:h}
typeset -g test_home=$(mktemp -d "${TMPDIR:-/tmp}/aicoach-onboarding.XXXXXX")
typeset -g daemon_pid=""

cleanup() {
  if [[ -n $daemon_pid ]]; then
    kill -TERM "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
  [[ -d $test_home ]] && rm -rf -- "$test_home"
}
trap cleanup EXIT INT TERM

command -v /usr/bin/expect >/dev/null
cargo build --quiet --locked -p aicoach-cli -p aicoach-daemon

mkdir -p "$test_home/.config/aicoach" "$test_home/.aicoach/run"
cp "$repo_root/config/default.toml" "$test_home/.config/aicoach/config.toml"
cp "$repo_root/shell/aicoach.zsh" "$test_home/.config/aicoach/aicoach.zsh"
builtin print -l -r -- \
  '# >>> AI Terminal Coach >>>' \
  '[[ -r "$HOME/.config/aicoach/aicoach.zsh" ]] && source "$HOME/.config/aicoach/aicoach.zsh"' \
  '# <<< AI Terminal Coach <<<' >| "$test_home/.zshrc"

export HOME=$test_home
typeset -gr cli=$repo_root/target/debug/aicoach
typeset -gr daemon=$repo_root/target/debug/aicoachd

[[ $($cli config path) == "$test_home/.config/aicoach/config.toml" ]]
$cli config validate >/dev/null
$daemon --foreground >| "$test_home/daemon.log" 2>&1 &
daemon_pid=$!

repeat 100; do
  [[ -S $test_home/.aicoach/run/aicoach.sock ]] && break
  sleep 0.05
done
[[ -S $test_home/.aicoach/run/aicoach.sock ]]

export AICOACH_ONBOARDING_CLI=$cli
/usr/bin/expect <<'EXPECT'
set timeout 15
log_user 0
spawn $env(AICOACH_ONBOARDING_CLI) onboard
expect "Choose \[1\]:"
send -- "\r"
expect "Press it now"
send -- "\033g"
expect "Press it now"
send -- "\033c"
expect "Press it now"
send -- "\033l"
expect "Setup complete"
expect eof
set result [wait]
exit [lindex $result 3]
EXPECT

typeset -g calibrated_config=$(<"$test_home/.config/aicoach/config.toml")
[[ $calibrated_config == *'completion = "^[g"'* ]]
[[ $calibrated_config == *'chat = "^[c"'* ]]
[[ $calibrated_config == *'risk_lens = "^[l"'* ]]
typeset -g generated_settings=$(<"$test_home/.config/aicoach/keybindings.zsh")
[[ $generated_settings == *"AICOACH_CONFIG_COMPLETION_KEY=\$'\\x1bg'"* ]]
$cli onboard --check >/dev/null

builtin print 'onboarding end-to-end test: ok'

//! Best-effort, asynchronous screen-tail capture for macOS Terminal.app/iTerm2.
//!
//! Shell-provided stdout/stderr remains authoritative. This module is only a
//! fallback for the lightweight Zsh protocol, which deliberately avoids piping
//! every command through a helper. Capture errors are never surfaced to the
//! shell and captured contents are never logged.

use std::{process::Stdio, time::Duration};

use tokio::{process::Command, time::timeout};

const MAX_SCREEN_TAIL_CHARS: usize = 20_000;
const CAPTURE_TIMEOUT: Duration = Duration::from_millis(750);

pub async fn capture_screen_tail(tty: &str, terminal_hint: Option<&str>) -> Option<String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (tty, terminal_hint);
        None
    }

    #[cfg(target_os = "macos")]
    {
        let kind = TerminalKind::from_hint(terminal_hint)?;
        let script = kind.script();
        let mut command = Command::new("/usr/bin/osascript");
        command
            .arg("-e")
            .arg(script)
            .arg(tty)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let output = timeout(CAPTURE_TIMEOUT, command.output())
            .await
            .ok()?
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let trimmed = text.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            return None;
        }
        Some(super::state::truncate_tail(trimmed, MAX_SCREEN_TAIL_CHARS))
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
enum TerminalKind {
    AppleTerminal,
    ITerm2,
}

#[cfg(target_os = "macos")]
impl TerminalKind {
    fn from_hint(hint: Option<&str>) -> Option<Self> {
        let hint = hint?;
        let lowercase = hint.to_ascii_lowercase();
        if lowercase.contains("iterm") {
            Some(Self::ITerm2)
        } else if lowercase.contains("apple_terminal") || lowercase.contains("terminal.app") {
            Some(Self::AppleTerminal)
        } else {
            None
        }
    }

    const fn script(self) -> &'static str {
        match self {
            Self::AppleTerminal => APPLE_TERMINAL_SCRIPT,
            Self::ITerm2 => ITERM2_SCRIPT,
        }
    }
}

#[cfg(target_os = "macos")]
const APPLE_TERMINAL_SCRIPT: &str = r#"
on run argv
  set wantedTty to item 1 of argv
  tell application "Terminal"
    repeat with w in windows
      repeat with t in tabs of w
        try
          if tty of t is wantedTty then return contents of t
        end try
      end repeat
    end repeat
  end tell
  return ""
end run
"#;

#[cfg(target_os = "macos")]
const ITERM2_SCRIPT: &str = r#"
on run argv
  set wantedTty to item 1 of argv
  tell application "iTerm2"
    repeat with w in windows
      repeat with t in tabs of w
        repeat with s in sessions of t
          try
            if tty of s is wantedTty then return contents of s
          end try
        end repeat
      end repeat
    end repeat
  end tell
  return ""
end run
"#;

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn terminal_hint_is_conservative() {
        assert!(matches!(
            TerminalKind::from_hint(Some("Apple_Terminal")),
            Some(TerminalKind::AppleTerminal)
        ));
        assert!(matches!(
            TerminalKind::from_hint(Some("iTerm.app")),
            Some(TerminalKind::ITerm2)
        ));
        assert!(TerminalKind::from_hint(Some("wezterm")).is_none());
        assert!(TerminalKind::from_hint(None).is_none());
    }
}

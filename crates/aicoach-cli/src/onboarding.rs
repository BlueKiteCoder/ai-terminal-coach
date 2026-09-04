use super::{
    DEFAULT_CONFIG, InstallArgs, Paths, SHELL_INTEGRATION, atomic_write,
    has_complete_managed_block, install, is_daemon_running, keybinding_sequence,
    keychain_key_exists, start, stop, write_shell_settings,
};
use anyhow::{Context, Result, bail};
use clap::Args;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    os::unix::ffi::OsStrExt,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const SHORTCUT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Args, Debug)]
pub(super) struct OnboardArgs {
    /// Verify the installed integration without changing files or reading keys.
    #[arg(long)]
    check: bool,
    /// Skip physical shortcut capture (useful in remote terminal sessions).
    #[arg(long)]
    skip_shortcuts: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Language {
    English,
    Chinese,
}

impl Language {
    fn from_config(value: &str) -> Self {
        if value == "zh-CN" {
            Self::Chinese
        } else {
            Self::English
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::English => "en-US",
            Self::Chinese => "zh-CN",
        }
    }

    fn text<'a>(self, english: &'a str, chinese: &'a str) -> &'a str {
        match self {
            Self::English => english,
            Self::Chinese => chinese,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShortcutKind {
    Completion,
    Chat,
    RiskLens,
}

impl ShortcutKind {
    fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::Completion, Language::English) => "AI completion — Option+Tab",
            (Self::Completion, Language::Chinese) => "AI 补全 — Option+Tab",
            (Self::Chat, Language::English) => "Ask AI — Option+/",
            (Self::Chat, Language::Chinese) => "询问 AI — Option+/",
            (Self::RiskLens, Language::English) => "Local Risk Lens — Option+R",
            (Self::RiskLens, Language::Chinese) => "本地风险透镜 — Option+R",
        }
    }

    fn widget(self) -> &'static str {
        match self {
            Self::Completion => "aicoach-complete",
            Self::Chat => "aicoach-chat",
            Self::RiskLens => "aicoach-risk-lens",
        }
    }

    fn configured(self, config: &aicoach_core::Config) -> &str {
        match self {
            Self::Completion => &config.keybindings.completion,
            Self::Chat => &config.keybindings.chat,
            Self::RiskLens => &config.keybindings.risk_lens,
        }
    }

    fn set_configured(self, config: &mut aicoach_core::Config, value: String) {
        match self {
            Self::Completion => config.keybindings.completion = value,
            Self::Chat => config.keybindings.chat = value,
            Self::RiskLens => config.keybindings.risk_lens = value,
        }
    }

    fn native_fallback(self) -> Option<&'static [u8]> {
        match self {
            Self::Completion => None,
            Self::Chat => Some("÷".as_bytes()),
            Self::RiskLens => Some("®".as_bytes()),
        }
    }
}

pub(super) fn run(paths: &Paths, args: &OnboardArgs) -> Result<()> {
    super::ensure_macos()?;
    if args.check {
        return check_installation(paths);
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(
            "interactive onboarding needs a terminal; use `aicoach onboard --check` for a non-interactive verification"
        );
    }

    println!("\x1b[1mAI Terminal Coach · two-minute setup / 两分钟设置\x1b[0m");
    println!(
        "No suggested shell command will be executed, and shortcut calibration makes no provider request.\n"
    );

    let integration = paths.config_dir.join("aicoach.zsh");
    let zshrc = fs::read_to_string(paths.home.join(".zshrc")).unwrap_or_default();
    let needs_install = !has_complete_managed_block(&zshrc)
        || !fs::read_to_string(&integration).is_ok_and(|contents| contents == SHELL_INTEGRATION);
    if needs_install {
        println!("1/4  Installing or refreshing the local integration…");
        install(
            paths,
            &InstallArgs {
                no_start: false,
                no_hotkey: false,
            },
        )?;
    } else {
        println!("1/4  \x1b[32m✓\x1b[0m Shell integration is current");
        if !is_daemon_running(paths).0 {
            start(paths)?;
        }
    }

    if !paths.config.exists() {
        atomic_write(&paths.config, DEFAULT_CONFIG, 0o600)?;
    }
    let mut config = aicoach_core::Config::load_from(&paths.config)
        .with_context(|| format!("load {}", paths.config.display()))?;
    let original_language = config.coach.language.clone();
    let language = choose_language(Language::from_config(&original_language))?;
    language.code().clone_into(&mut config.coach.language);

    println!(
        "\n2/4  {}",
        language.text(
            "Calibrating the keys your terminal actually sends",
            "校准终端实际发送的按键"
        )
    );
    let mut bindings_changed = false;
    if args.skip_shortcuts {
        println!(
            "     {}",
            language.text(
                "Skipped physical capture; configured bindings will still be checked.",
                "已跳过实际按键采集；仍会检查配置的绑定。"
            )
        );
    } else {
        for kind in [
            ShortcutKind::Completion,
            ShortcutKind::Chat,
            ShortcutKind::RiskLens,
        ] {
            if calibrate_shortcut(&mut config, kind, language)? {
                bindings_changed = true;
            }
        }
    }

    config.validate()?;
    let language_changed = original_language != config.coach.language;
    if language_changed || bindings_changed {
        config.save_to(&paths.config)?;
    }
    // Always regenerate this file so installations made by an older release
    // gain the reload-safe variable format and a fresh settings generation.
    write_shell_settings(paths)?;

    println!(
        "\n3/4  {}",
        language.text("Verifying Zsh wiring", "验证 Zsh 绑定")
    );
    let shortcuts_ok = verify_shortcuts(paths, &config, language);

    if language_changed {
        // Language affects daemon-side local analysis as well as shell text.
        stop(paths)?;
        start(paths)?;
    }

    println!(
        "\n4/4  {}",
        language.text("Privacy and provider status", "隐私与 AI 服务状态")
    );
    print_provider_status(&config, language);

    if !shortcuts_ok {
        bail!(
            "{}",
            language.text(
                "one or more Zsh bindings could not be verified; run `aicoach doctor` for the remaining checks",
                "一个或多个 Zsh 绑定未通过验证；请运行 `aicoach doctor` 查看其余检查"
            )
        );
    }

    println!(
        "\n\x1b[32m✓ {}\x1b[0m",
        language.text("Setup complete", "设置完成")
    );
    if needs_install {
        println!(
            "{}",
            language.text(
                "Open one new terminal tab to load the refreshed integration.",
                "请新开一个终端标签页，以加载更新后的集成。"
            )
        );
    } else {
        println!(
            "{}",
            language.text(
                "Changed shortcuts reload automatically at the next prompt.",
                "变更后的快捷键会在下一个提示符自动重载。"
            )
        );
    }
    println!(
        "{}",
        language.text(
            "First safe test: type `git reset --hard` without pressing Enter, then press Option+R.",
            "第一次安全体验：输入 `git reset --hard`，不要按 Enter，然后按 Option+R。"
        )
    );
    Ok(())
}

fn choose_language(current: Language) -> Result<Language> {
    println!("\nLanguage / 语言：");
    println!("  1. English");
    println!("  2. 简体中文");
    let default = if current == Language::Chinese {
        "2"
    } else {
        "1"
    };
    print!("Choose [{default}]: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    match input.trim() {
        "" => Ok(current),
        "1" | "en" | "en-US" => Ok(Language::English),
        "2" | "zh" | "zh-CN" => Ok(Language::Chinese),
        value => bail!("unsupported language selection `{value}`; choose 1 or 2"),
    }
}

fn calibrate_shortcut(
    config: &mut aicoach_core::Config,
    kind: ShortcutKind,
    language: Language,
) -> Result<bool> {
    println!("\n     {}", kind.label(language));
    println!(
        "     {}",
        language.text(
            "Press it now (Esc keeps the current setting).",
            "现在按下该组合键（按 Esc 保留当前设置）。"
        )
    );
    let Some(sequence) = capture_shortcut()? else {
        println!(
            "     ! {}",
            language.text("kept current setting", "已保留当前设置")
        );
        return Ok(false);
    };
    println!("     → {}", describe_sequence(&sequence));

    if !is_safe_shortcut(&sequence) {
        println!(
            "     \x1b[33m! {}\x1b[0m",
            language.text(
                "the modifier was not distinguishable from normal typing; enable “Use Option as Meta key” and rerun onboarding",
                "终端没有把修饰键与普通输入区分开；请启用“Use Option as Meta key”后重新运行设置"
            )
        );
        return Ok(false);
    }

    let configured = keybinding_sequence(kind.configured(config))?;
    if sequence == configured || kind.native_fallback() == Some(sequence.as_slice()) {
        println!(
            "     \x1b[32m✓ {}\x1b[0m",
            language.text("already compatible", "已经兼容")
        );
        return Ok(false);
    }

    let previous = kind.configured(config).to_owned();
    let specification = shortcut_specification(&sequence)?;
    kind.set_configured(config, specification);
    if let Err(error) = config.validate() {
        kind.set_configured(config, previous);
        println!(
            "     \x1b[33m! {}: {error}\x1b[0m",
            language.text("not saved because it conflicts", "因快捷键冲突而未保存")
        );
        return Ok(false);
    }
    println!(
        "     \x1b[32m✓ {}\x1b[0m",
        language.text("calibrated and saved", "已校准并保存")
    );
    Ok(true)
}

fn capture_shortcut() -> Result<Option<Vec<u8>>> {
    let guard = RawModeGuard::enable()?;
    let deadline = Instant::now() + SHORTCUT_TIMEOUT;
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            drop(guard);
            println!();
            return Ok(None);
        };
        if !event::poll(remaining)? {
            drop(guard);
            println!();
            return Ok(None);
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            drop(guard);
            println!();
            return Ok(None);
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            drop(guard);
            println!();
            bail!("onboarding cancelled");
        }
        let Some(sequence) = key_event_sequence(key) else {
            drop(guard);
            println!();
            bail!("that key cannot be represented safely as a Zsh binding");
        };
        drop(guard);
        println!();
        return Ok(Some(sequence));
    }
}

fn key_event_sequence(key: KeyEvent) -> Option<Vec<u8>> {
    let mut sequence = Vec::new();
    if key.modifiers.contains(KeyModifiers::ALT) {
        sequence.push(0x1b);
    }
    match key.code {
        KeyCode::Char(character) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if !character.is_ascii() {
                return None;
            }
            sequence.push(character.to_ascii_uppercase() as u8 & 0x1f);
        }
        KeyCode::Char(character) => {
            let mut encoded = [0; 4];
            sequence.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        }
        KeyCode::Tab => sequence.push(b'\t'),
        KeyCode::Enter => sequence.push(b'\r'),
        KeyCode::Backspace => sequence.push(0x7f),
        KeyCode::Esc => sequence.push(0x1b),
        _ => return None,
    }
    Some(sequence)
}

fn is_safe_shortcut(sequence: &[u8]) -> bool {
    if sequence.len() > 16
        || sequence.is_empty()
        || sequence.contains(&0)
        || sequence.contains(&b'\r')
        || sequence.contains(&b'\n')
    {
        return false;
    }
    if sequence.starts_with(&[0x1b]) {
        return sequence.len() > 1;
    }
    let Ok(value) = std::str::from_utf8(sequence) else {
        return false;
    };
    let mut characters = value.chars();
    let Some(character) = characters.next() else {
        return false;
    };
    characters.next().is_none()
        && !character.is_ascii()
        && !character.is_control()
        && !character.is_whitespace()
}

fn shortcut_specification(sequence: &[u8]) -> Result<String> {
    match sequence {
        [0x1b, b'\t'] => return Ok("Option+Tab".to_owned()),
        [0x1b, b'/'] => return Ok("Option+/".to_owned()),
        [0x1b, b'r'] => return Ok("Option+R".to_owned()),
        _ => {}
    }
    let (prefix, bytes) = if sequence.starts_with(&[0x1b]) {
        ("^[", &sequence[1..])
    } else {
        ("", sequence)
    };
    let value = std::str::from_utf8(bytes).context("shortcut is not valid UTF-8")?;
    let escaped = value.replace('\\', "\\\\").replace('\t', "\\t");
    Ok(format!("{prefix}{escaped}"))
}

fn describe_sequence(sequence: &[u8]) -> String {
    if let Some(rest) = sequence.strip_prefix(&[0x1b]) {
        if rest == b"\t" {
            return "Esc + Tab (Meta sequence 1b 09)".to_owned();
        }
        if let Ok(value) = std::str::from_utf8(rest) {
            return format!("Esc + {value:?} (Meta sequence {})", hex_bytes(sequence));
        }
    }
    if let Ok(value) = std::str::from_utf8(sequence) {
        return format!("{value:?} (UTF-8 {})", hex_bytes(sequence));
    }
    format!("bytes {}", hex_bytes(sequence))
}

fn hex_bytes(sequence: &[u8]) -> String {
    sequence
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn verify_shortcuts(paths: &Paths, config: &aicoach_core::Config, language: Language) -> bool {
    let integration = paths.config_dir.join("aicoach.zsh");
    let mut all_ok = true;
    for kind in [
        ShortcutKind::Completion,
        ShortcutKind::Chat,
        ShortcutKind::RiskLens,
    ] {
        let sequence = match keybinding_sequence(kind.configured(config)) {
            Ok(sequence) => sequence,
            Err(error) => {
                println!("     \x1b[31m✗\x1b[0m {}: {error}", kind.label(language));
                all_ok = false;
                continue;
            }
        };
        let works = verify_zsh_binding(&integration, &sequence, kind.widget());
        print_wiring_result(kind.label(language), kind.widget(), works);
        all_ok &= works;
    }
    let toggle = keybinding_sequence(&config.keybindings.toggle_coach).unwrap_or_default();
    let toggle_works = verify_zsh_binding(&integration, &toggle, "aicoach-toggle");
    print_wiring_result(
        language.text("Coach window — Option+Space", "Coach 窗口 — Option+Space"),
        "aicoach-toggle",
        toggle_works,
    );
    all_ok & toggle_works
}

fn print_wiring_result(label: &str, widget: &str, works: bool) {
    if works {
        println!("     \x1b[32m✓\x1b[0m {label} → {widget}");
    } else {
        println!("     \x1b[31m✗\x1b[0m {label} → {widget}");
    }
}

fn verify_zsh_binding(integration: &Path, sequence: &[u8], widget: &str) -> bool {
    if !integration.is_file() || sequence.is_empty() || sequence.contains(&0) {
        return false;
    }
    let Some(config_dir) = integration.parent() else {
        return false;
    };
    let output = Command::new("/bin/zsh")
        .args([
            "-dfc",
            "source \"$AICOACH_VERIFY_SCRIPT\"; bindkey -M emacs \"$AICOACH_VERIFY_SEQUENCE\"",
        ])
        .env("AICOACH_TEST_MODE", "1")
        .env("AICOACH_VERIFY_SCRIPT", integration)
        .env("AICOACH_SETTINGS_FILE", config_dir.join("keybindings.zsh"))
        .env(
            "AICOACH_SETTINGS_VERSION_FILE",
            config_dir.join("keybindings.version"),
        )
        .env(
            "AICOACH_VERIFY_SEQUENCE",
            std::ffi::OsStr::from_bytes(sequence),
        )
        .env_remove("AICOACH_COMPLETION_KEY")
        .env_remove("AICOACH_CHAT_KEY")
        .env_remove("AICOACH_RISK_LENS_KEY")
        .env_remove("AICOACH_TOGGLE_KEY")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    output.is_ok_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .ends_with(widget)
    })
}

fn print_provider_status(config: &aicoach_core::Config, language: Language) {
    if config.ai.provider == "disabled" {
        println!(
            "     \x1b[32m✓\x1b[0m {}",
            language.text(
                "Local-only mode is ready; Risk Lens needs no key and sends nothing to a provider.",
                "本地模式已经可用；Risk Lens 不需要密钥，也不会向服务商发送内容。"
            )
        );
        println!(
            "     {}",
            language.text(
                "AI is optional. Configure a provider, then store its key with `aicoach config set-key`.",
                "AI 功能可选。配置服务商后，使用 `aicoach config set-key` 安全保存密钥。"
            )
        );
        return;
    }
    let key_env = &config.ai.api_key_env;
    let has_key =
        env::var_os(key_env).is_some_and(|value| !value.is_empty()) || keychain_key_exists();
    if has_key {
        println!(
            "     \x1b[32m✓\x1b[0m {}",
            language.text(
                "Provider configuration and credential are present (no network probe was made).",
                "服务商配置和密钥均已就绪（未进行网络探测）。"
            )
        );
    } else {
        println!(
            "     \x1b[33m! {}\x1b[0m",
            language.text(
                "Provider is enabled but its credential is missing; run `aicoach config set-key`.",
                "服务商已启用，但缺少密钥；请运行 `aicoach config set-key`。"
            )
        );
    }
}

fn check_installation(paths: &Paths) -> Result<()> {
    let integration = paths.config_dir.join("aicoach.zsh");
    let zshrc = fs::read_to_string(paths.home.join(".zshrc")).unwrap_or_default();
    let source_ok = has_complete_managed_block(&zshrc);
    let integration_ok =
        fs::read_to_string(&integration).is_ok_and(|contents| contents == SHELL_INTEGRATION);
    print_wiring_result("managed ~/.zshrc source block", "installed", source_ok);
    print_wiring_result("current Zsh integration", "installed", integration_ok);

    let config = aicoach_core::Config::load_from(&paths.config)
        .with_context(|| format!("load {}", paths.config.display()))?;
    println!("\x1b[32m✓\x1b[0m configuration validates");
    let language = Language::from_config(&config.coach.language);
    let shortcut_ok = if integration_ok {
        verify_shortcuts(paths, &config, language)
    } else {
        println!(
            "     \x1b[33m!\x1b[0m {}",
            language.text(
                "shortcut wiring check skipped until the integration is refreshed",
                "快捷键绑定检查已跳过；请先更新 Shell 集成"
            )
        );
        false
    };
    let daemon_ok = is_daemon_running(paths).0;
    print_wiring_result("daemon socket", "ready", daemon_ok);
    print_provider_status(&config, language);
    if source_ok && integration_ok && shortcut_ok && daemon_ok {
        println!("\x1b[32m✓ onboarding check passed\x1b[0m");
        Ok(())
    } else {
        bail!("onboarding check found required items that need attention")
    }
}

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self> {
        enable_raw_mode().context("enable terminal raw mode")?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_key_events_become_zsh_sequences() {
        assert_eq!(
            key_event_sequence(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::ALT)),
            Some(vec![0x1b, b'/'])
        );
        assert_eq!(
            key_event_sequence(KeyEvent::new(KeyCode::Tab, KeyModifiers::ALT)),
            Some(vec![0x1b, b'\t'])
        );
        assert_eq!(
            key_event_sequence(KeyEvent::new(KeyCode::Char('÷'), KeyModifiers::NONE)),
            Some("÷".as_bytes().to_vec())
        );
    }

    #[test]
    fn normal_typing_and_reserved_keys_are_never_saved() {
        assert!(!is_safe_shortcut(b"r"));
        assert!(!is_safe_shortcut(b"/"));
        assert!(!is_safe_shortcut(b"\t"));
        assert!(!is_safe_shortcut(b"\r"));
        assert!(is_safe_shortcut(b"\x1br"));
        assert!(is_safe_shortcut("®".as_bytes()));
    }

    #[test]
    fn captured_shortcuts_round_trip_through_existing_parser() {
        for sequence in [
            vec![0x1b, b'\t'],
            vec![0x1b, b'/'],
            vec![0x1b, b'g'],
            "÷".as_bytes().to_vec(),
        ] {
            let specification = shortcut_specification(&sequence).unwrap();
            assert_eq!(keybinding_sequence(&specification).unwrap(), sequence);
        }
    }

    #[test]
    fn a_recorded_collision_is_rejected_by_config_validation() {
        let mut config = aicoach_core::Config::default();
        ShortcutKind::RiskLens.set_configured(&mut config, "Option+/".to_owned());
        assert!(config.validate().is_err());
    }

    #[test]
    fn clean_zsh_process_confirms_generated_widget_wiring() {
        let directory = tempfile::tempdir().unwrap();
        let integration = directory.path().join("aicoach.zsh");
        fs::write(&integration, SHELL_INTEGRATION).unwrap();
        fs::write(
            directory.path().join("keybindings.zsh"),
            "typeset -g AICOACH_CONFIG_COMPLETION_KEY=$'\\eg'\n\
             typeset -g AICOACH_CONFIG_CHAT_KEY=$'\\ec'\n\
             typeset -g AICOACH_CONFIG_RISK_LENS_KEY=$'\\el'\n\
             typeset -g AICOACH_CONFIG_TOGGLE_KEY=$'\\e '\n",
        )
        .unwrap();
        fs::write(directory.path().join("keybindings.version"), "1\n").unwrap();

        assert!(verify_zsh_binding(
            &integration,
            b"\x1bg",
            "aicoach-complete"
        ));
        assert!(verify_zsh_binding(&integration, b"\x1bc", "aicoach-chat"));
        assert!(verify_zsh_binding(
            &integration,
            b"\x1bl",
            "aicoach-risk-lens"
        ));
        assert!(!verify_zsh_binding(
            &integration,
            b"\x1bc",
            "aicoach-complete"
        ));
    }
}

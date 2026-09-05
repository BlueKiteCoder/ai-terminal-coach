#![allow(clippy::items_after_statements, clippy::too_many_lines)]

mod capsule;
mod checkpoint;
mod data;
mod onboarding;
mod support;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::{
    env, fs,
    fs::OpenOptions,
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SHELL_INTEGRATION: &str = include_str!("../../../shell/aicoach.zsh");
const DEFAULT_CONFIG: &str = include_str!("../../../config/default.toml");
const WINDOW_SCRIPT: &str = include_str!("../../../scripts/aicoach-window.js");
const HIDE_SCRIPT: &str = include_str!("../../../scripts/aicoach-hide.js");
const MANAGED_START: &str = "# >>> AI Terminal Coach >>>";
const MANAGED_END: &str = "# <<< AI Terminal Coach <<<";
const DAEMON_LABEL: &str = "com.aicoach.daemon";
const HOTKEY_LABEL: &str = "com.aicoach.hotkey";

#[derive(Parser, Debug)]
#[command(
    name = "aicoach",
    version,
    about = "AI Terminal Coach — AI collaboration inside your macOS terminal",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the two-minute setup and calibrate physical Option shortcuts.
    Onboard(onboarding::OnboardArgs),
    /// Install the Zsh integration and macOS background services.
    Install(InstallArgs),
    /// Remove integration while preserving config, local memory, and logs.
    Uninstall(UninstallArgs),
    /// Start (or wake) the daemon.
    Start,
    /// Stop the daemon.
    Stop,
    /// Restart the daemon.
    Restart,
    /// Show daemon and session status.
    Status(OutputArgs),
    /// Diagnose configuration, shell, socket, AI credential and terminal integration.
    Doctor(OutputArgs),
    /// Export a path-free, content-free Markdown report for public support requests.
    Support(support::SupportArgs),
    /// Inspect or modify configuration.
    Config(ConfigArgs),
    /// Read bounded daemon logs.
    Logs(LogsArgs),
    /// Export a private-by-default Markdown capsule of the active terminal session.
    Capsule(CapsuleArgs),
    /// Inspect or clear bounded, local-only failure fingerprints.
    Memory(MemoryArgs),
    /// Name, resolve, or clear the active terminal troubleshooting checkpoint.
    Checkpoint(CheckpointArgs),
    /// Inventory and precisely clear local data without exposing its contents.
    Data(DataArgs),
    /// Toggle the native Terminal.app/iTerm2 Coach window.
    Toggle(ToggleArgs),
}

#[derive(Args, Debug)]
struct InstallArgs {
    /// Do not start the daemon after installing.
    #[arg(long)]
    no_start: bool,
    /// Do not install the optional global Option+Space helper.
    #[arg(long)]
    no_hotkey: bool,
}

#[derive(Args, Debug)]
struct UninstallArgs {
    /// Also remove config, history and logs. This cannot be undone.
    #[arg(long)]
    purge: bool,
}

#[derive(Args, Debug)]
struct OutputArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct ConfigArgs {
    #[command(subcommand)]
    action: Option<ConfigAction>,
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    /// Print the active configuration.
    Show,
    /// Print the active configuration path.
    Path,
    /// Validate the active configuration without revealing secrets.
    Validate,
    /// Set a dotted TOML key, for example ai.models.chat.
    Set { key: String, value: String },
    /// Open the configuration in $VISUAL or $EDITOR.
    Edit,
    /// Store the API key in the macOS Keychain without writing it to disk.
    SetKey,
    /// Remove the API key from the macOS Keychain.
    DeleteKey,
}

#[derive(Args, Debug)]
struct LogsArgs {
    /// Follow new log lines.
    #[arg(short, long)]
    follow: bool,
    /// Number of existing lines to print.
    #[arg(short = 'n', long, default_value_t = 100)]
    lines: usize,
}

#[derive(Args, Debug)]
struct ToggleArgs {
    #[arg(long, default_value = "")]
    session: String,
    #[arg(long, default_value = "")]
    tty: String,
}

#[derive(Args, Debug)]
struct CapsuleArgs {
    /// Shell session UUID. Defaults to the most recently focused terminal.
    #[arg(long, default_value = "")]
    session: String,
    /// Number of recent commands to request from the daemon.
    #[arg(long, default_value_t = 20, value_parser = capsule::parse_capsule_limit)]
    last: usize,
    /// Include only commands whose exit status was non-zero.
    #[arg(long)]
    failed_only: bool,
    /// Copy the generated Markdown to the macOS clipboard.
    #[arg(long)]
    copy: bool,
    /// Write the generated Markdown to a private file instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct MemoryArgs {
    #[command(subcommand)]
    action: Option<MemoryAction>,
}

#[derive(Subcommand, Debug)]
enum MemoryAction {
    /// Show retention settings and the exact local data path.
    Status(OutputArgs),
    /// List retained, already-redacted successful follow-up commands.
    List(OutputArgs),
    /// Delete every retained failure fingerprint and restart the daemon if needed.
    Clear,
}

#[derive(Args, Debug)]
struct CheckpointArgs {
    /// Shell session UUID. Defaults to the most recently focused terminal.
    #[arg(long, default_value = "")]
    session: String,
    #[command(subcommand)]
    action: Option<CheckpointAction>,
}

#[derive(Subcommand, Debug)]
enum CheckpointAction {
    /// Start or replace a named, memory-only troubleshooting checkpoint.
    Start { name: String },
    /// Record the final resolution for the active checkpoint.
    Resolve {
        /// Resolution text. Omit it to enter the text outside Shell history.
        resolution: Option<String>,
    },
    /// Show the active checkpoint and its privacy boundary.
    Status(OutputArgs),
    /// Clear the active checkpoint without deleting terminal command context.
    Clear,
}

#[derive(Args, Debug)]
struct DataArgs {
    #[command(subcommand)]
    action: Option<DataAction>,
}

#[derive(Subcommand, Debug)]
enum DataAction {
    /// Show every product-managed persistent and in-memory data category.
    Status(OutputArgs),
    /// List per-session daemon memory counts without command or chat content.
    Sessions(OutputArgs),
    /// Delete one explicit local-data scope.
    Clear(DataClearArgs),
}

#[derive(Args, Debug)]
struct DataClearArgs {
    #[arg(value_enum)]
    scope: DataScope,
    /// Shell session UUID for the session scope. Defaults to the active terminal.
    #[arg(long, default_value = "")]
    session: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum DataScope {
    Session,
    History,
    Fingerprints,
    Logs,
    All,
}

#[derive(Clone, Debug)]
struct Paths {
    home: PathBuf,
    config_dir: PathBuf,
    config: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
    run_dir: PathBuf,
    logs_dir: PathBuf,
    failure_memory: PathBuf,
    history: PathBuf,
    window_state: PathBuf,
    socket: PathBuf,
    pid: PathBuf,
    launch_agents: PathBuf,
    daemon_plist: PathBuf,
    hotkey_plist: PathBuf,
}

impl Paths {
    fn discover() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
        Ok(Self::from_home(home))
    }

    fn from_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let config_dir = home.join(".config/aicoach");
        let state_dir = home.join(".aicoach");
        let data_dir = config_dir.join("assets");
        let run_dir = state_dir.join("run");
        let logs_dir = state_dir.join("logs");
        let launch_agents = home.join("Library/LaunchAgents");
        Self {
            home,
            config: config_dir.join("config.toml"),
            data_dir,
            failure_memory: state_dir.join("failure-memory.json"),
            history: state_dir.join("history.json"),
            window_state: state_dir.join("window-state.json"),
            socket: run_dir.join("aicoach.sock"),
            pid: run_dir.join("aicoachd.pid"),
            daemon_plist: launch_agents.join(format!("{DAEMON_LABEL}.plist")),
            hotkey_plist: launch_agents.join(format!("{HOTKEY_LABEL}.plist")),
            config_dir,
            state_dir,
            run_dir,
            logs_dir,
            launch_agents,
        }
    }

    fn create_runtime(&self) -> Result<()> {
        secure_dir(&self.config_dir)?;
        secure_dir(&self.data_dir)?;
        secure_dir(&self.state_dir)?;
        secure_dir(&self.run_dir)?;
        secure_dir(&self.logs_dir)?;
        fs::create_dir_all(&self.launch_agents)
            .with_context(|| format!("create {}", self.launch_agents.display()))?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct StatusReport {
    installed: bool,
    daemon_running: bool,
    socket_ready: bool,
    pid: Option<u32>,
    config: String,
    active_session: Option<String>,
}

#[derive(Debug, Serialize)]
struct Check {
    name: &'static str,
    status: &'static str,
    detail: String,
    required: bool,
}

#[derive(Debug, Serialize)]
struct MemoryStatusReport {
    enabled: bool,
    path: String,
    exists: bool,
    bytes: u64,
    entries: usize,
    max_entries: usize,
    retention_days: u64,
    resolution_window_minutes: u64,
    persisted_fields: [&'static str; 6],
    excluded_fields: [&'static str; 3],
}

fn main() {
    if let Err(error) = run() {
        eprintln!("\x1b[31merror:\x1b[0m {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::discover()?;
    match cli.command {
        Commands::Onboard(args) => onboarding::run(&paths, &args),
        Commands::Install(args) => install(&paths, &args),
        Commands::Uninstall(args) => uninstall(&paths, &args),
        Commands::Start => start(&paths),
        Commands::Stop => stop(&paths),
        Commands::Restart => {
            stop(&paths)?;
            thread::sleep(Duration::from_millis(250));
            start(&paths)
        }
        Commands::Status(args) => status(&paths, args.json),
        Commands::Doctor(args) => doctor(&paths, args.json),
        Commands::Support(args) => support::export(&paths, &args),
        Commands::Config(args) => config_command(&paths, args.action),
        Commands::Logs(args) => logs(&paths, &args),
        Commands::Capsule(args) => capsule::export(&paths, &args),
        Commands::Memory(args) => memory_command(&paths, args.action),
        Commands::Checkpoint(args) => checkpoint::run(&paths, &args),
        Commands::Data(args) => data::run(&paths, &args),
        Commands::Toggle(args) => toggle(&paths, &args),
    }
}

fn install(paths: &Paths, args: &InstallArgs) -> Result<()> {
    ensure_macos()?;
    paths.create_runtime()?;
    atomic_write(
        &paths.config_dir.join("aicoach.zsh"),
        SHELL_INTEGRATION,
        0o600,
    )?;
    atomic_write(
        &paths.data_dir.join("aicoach-window.js"),
        WINDOW_SCRIPT,
        0o600,
    )?;
    atomic_write(&paths.data_dir.join("aicoach-hide.js"), HIDE_SCRIPT, 0o600)?;
    if !paths.config.exists() {
        atomic_write(&paths.config, DEFAULT_CONFIG, 0o600)?;
        println!("created {}", paths.config.display());
    }
    write_shell_settings(paths)?;

    install_zshrc(paths)?;
    write_direct_daemon_plist(paths)?;
    let cli = sibling_executable("aicoach")?;
    let path_env = executable_path_env(&cli);

    let helper_installed = if args.no_hotkey {
        stop_agent(&paths.hotkey_plist, HOTKEY_LABEL);
        remove_file_if_exists(&paths.hotkey_plist)?;
        false
    } else if let Ok(helper) = sibling_executable("aicoach-hotkey") {
        let plist = launch_agent_plist(
            HOTKEY_LABEL,
            &helper,
            &[],
            true,
            true,
            &paths.logs_dir.join("hotkey.log"),
            &path_env,
        );
        atomic_write(&paths.hotkey_plist, &plist, 0o600)?;
        true
    } else {
        false
    };

    if !args.no_start {
        start(paths)?;
        if helper_installed {
            bootstrap_agent(&paths.hotkey_plist, HOTKEY_LABEL, false)?;
        }
    }

    println!("\x1b[32mAI Terminal Coach installed.\x1b[0m");
    println!(
        "Open a new Zsh session, or run: source {}",
        paths.config_dir.join("aicoach.zsh").display()
    );
    println!("Next: run `aicoach onboard` to verify the shortcuts your terminal actually sends.");
    if !helper_installed && !args.no_hotkey {
        println!(
            "\x1b[33mGlobal hotkey helper was not found; terminal-local Option+Space remains available.\x1b[0m"
        );
    }
    Ok(())
}

fn uninstall(paths: &Paths, args: &UninstallArgs) -> Result<()> {
    stop_agent(&paths.hotkey_plist, HOTKEY_LABEL);
    stop(paths)?;
    let zshrc = paths.home.join(".zshrc");
    if zshrc.exists() {
        let original = fs::read_to_string(&zshrc).context("read ~/.zshrc")?;
        let updated = remove_managed_block(&original);
        if updated != original {
            atomic_write(&zshrc, &updated, file_mode(&zshrc).unwrap_or(0o600))?;
        }
    }
    remove_file_if_exists(&paths.daemon_plist)?;
    remove_file_if_exists(&paths.hotkey_plist)?;
    remove_file_if_exists(&paths.config_dir.join("aicoach.zsh"))?;
    remove_file_if_exists(&paths.config_dir.join("keybindings.zsh"))?;
    remove_file_if_exists(&paths.config_dir.join("keybindings.version"))?;
    remove_file_if_exists(&paths.data_dir.join("aicoach-window.js"))?;
    remove_file_if_exists(&paths.data_dir.join("aicoach-hide.js"))?;
    remove_file_if_exists(&paths.data_dir.join("aicoachd-keychain"))?;
    remove_dir_if_empty(&paths.data_dir);

    if args.purge {
        // Both roots are resolved from the current user's home and validated to
        // have the expected final component before recursive removal.
        validate_purge_target(&paths.config_dir, "aicoach")?;
        validate_purge_target(&paths.state_dir, ".aicoach")?;
        if paths.config_dir.exists() {
            fs::remove_dir_all(&paths.config_dir).context("purge config")?;
        }
        if paths.state_dir.exists() {
            fs::remove_dir_all(&paths.state_dir).context("purge state")?;
        }
        println!(
            "AI Terminal Coach and local data were removed; shell backup files were preserved."
        );
    } else {
        println!(
            "AI Terminal Coach integration removed. Config, local memory, and logs were preserved."
        );
    }
    Ok(())
}

fn start(paths: &Paths) -> Result<()> {
    ensure_macos()?;
    paths.create_runtime()?;
    let key_env = configured_key_env(paths)?;
    let has_environment_key = env::var_os(&key_env).is_some_and(|value| !value.is_empty());
    let has_keychain_key = keychain_key_exists();
    if has_keychain_key {
        write_keychain_wrapper(paths)?;
    }
    if ping_socket(&paths.socket) {
        println!("aicoachd is already running");
        return Ok(());
    }

    if has_environment_key && !has_keychain_key {
        // launchd does not inherit variables exported by the invoking shell.
        // A detached child does, and the daemon writes its own rolling log.
        stop_agent(&paths.daemon_plist, DAEMON_LABEL);
        let daemon = sibling_executable("aicoachd")?;
        Command::new(&daemon)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("start {}", daemon.display()))?;
    } else if paths.daemon_plist.exists() {
        bootstrap_agent(&paths.daemon_plist, DAEMON_LABEL, true)?;
    } else {
        let daemon = sibling_executable("aicoachd")?;
        Command::new(&daemon)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("start {}", daemon.display()))?;
    }

    // Keychain access through the launchd wrapper can take several seconds on
    // the first request after login. Wait long enough for that normal path,
    // while still returning immediately as soon as the socket is available.
    for _ in 0..100 {
        if paths.socket.exists() {
            println!("aicoachd started");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    bail!(
        "daemon did not create {} within 10 seconds; inspect `aicoach logs`",
        paths.socket.display()
    )
}

fn stop(paths: &Paths) -> Result<()> {
    let _ = request_shutdown(&paths.socket);
    stop_agent(&paths.daemon_plist, DAEMON_LABEL);
    let (running, pid) = is_daemon_running(paths);
    if running && let Some(pid) = pid {
        let _ = Command::new("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
    for _ in 0..20 {
        if !is_daemon_running(paths).0 {
            remove_file_if_exists(&paths.socket)?;
            remove_file_if_exists(&paths.pid)?;
            println!("aicoachd stopped");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    if running {
        bail!("daemon did not stop cleanly; PID was {pid:?}")
    }
    println!("aicoachd is not running");
    Ok(())
}

fn status(paths: &Paths, as_json: bool) -> Result<()> {
    let (running, pid) = is_daemon_running(paths);
    let active_session = fs::read_to_string(paths.run_dir.join("active-session"))
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let report = StatusReport {
        installed: paths.config_dir.join("aicoach.zsh").exists(),
        daemon_running: running,
        socket_ready: paths.socket.exists() && ping_socket(&paths.socket),
        pid,
        config: paths.config.display().to_string(),
        active_session,
    };
    if as_json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("AI Terminal Coach");
        println!("  installed: {}", yes_no(report.installed));
        println!(
            "  daemon:    {}{}",
            if report.daemon_running {
                "running"
            } else {
                "stopped"
            },
            report
                .pid
                .map_or_else(String::new, |pid| format!(" (pid {pid})"))
        );
        println!(
            "  IPC:       {}",
            if report.socket_ready {
                "ready"
            } else {
                "unavailable"
            }
        );
        println!("  config:    {}", report.config);
        println!(
            "  session:   {}",
            report.active_session.as_deref().unwrap_or("none")
        );
    }
    Ok(())
}

fn doctor(paths: &Paths, as_json: bool) -> Result<()> {
    let checks = collect_doctor_checks(paths);
    if as_json {
        println!("{}", serde_json::to_string_pretty(&checks)?);
    } else {
        for check in &checks {
            let icon = match check.status {
                "ok" => "\x1b[32m✓\x1b[0m",
                "warn" => "\x1b[33m!\x1b[0m",
                _ => "\x1b[31m✗\x1b[0m",
            };
            println!("{icon} {:<18} {}", check.name, check.detail);
        }
    }
    if checks
        .iter()
        .any(|check| check.required && check.status == "fail")
    {
        bail!("doctor found required checks that need attention")
    }
    Ok(())
}

fn collect_doctor_checks(paths: &Paths) -> Vec<Check> {
    let config_text = fs::read_to_string(&paths.config).ok();
    let config_value = config_text
        .as_deref()
        .and_then(|text| toml::from_str::<toml::Value>(text).ok());
    let key_env = config_value
        .as_ref()
        .and_then(|value| value.get("ai"))
        .and_then(|value| value.get("api_key_env"))
        .and_then(toml::Value::as_str)
        .unwrap_or("AI_COACH_API_KEY");
    let (running, _) = is_daemon_running(paths);
    let zshrc = fs::read_to_string(paths.home.join(".zshrc")).unwrap_or_default();
    let terminal = env::var("TERM_PROGRAM").unwrap_or_else(|_| "unknown".to_owned());
    let mut checks = vec![
        Check {
            name: "macOS",
            status: if cfg!(target_os = "macos") {
                "ok"
            } else {
                "fail"
            },
            detail: env::consts::OS.to_owned(),
            required: true,
        },
        Check {
            name: "Zsh integration",
            status: if has_complete_managed_block(&zshrc)
                && paths.config_dir.join("aicoach.zsh").exists()
            {
                "ok"
            } else {
                "fail"
            },
            detail: paths.config_dir.join("aicoach.zsh").display().to_string(),
            required: true,
        },
        Check {
            name: "Config",
            status: if config_value.is_some() { "ok" } else { "fail" },
            detail: paths.config.display().to_string(),
            required: true,
        },
        Check {
            name: "Daemon",
            status: if running { "ok" } else { "fail" },
            detail: if running {
                "running".to_owned()
            } else {
                "not running".to_owned()
            },
            required: true,
        },
        Check {
            name: "Socket",
            status: if paths.socket.exists() && ping_socket(&paths.socket) {
                "ok"
            } else {
                "fail"
            },
            detail: paths.socket.display().to_string(),
            required: true,
        },
        Check {
            name: "AI credential",
            status: if env::var_os(key_env).is_some_and(|v| !v.is_empty()) || keychain_key_exists()
            {
                "ok"
            } else {
                "warn"
            },
            detail: if env::var_os(key_env).is_some_and(|v| !v.is_empty()) {
                format!("environment variable {key_env} is set")
            } else if keychain_key_exists() {
                "stored in macOS Keychain".to_owned()
            } else {
                format!("environment variable {key_env} is not set")
            },
            required: false,
        },
        Check {
            name: "Terminal",
            status: if public_terminal_name(&terminal).is_some() {
                "ok"
            } else {
                "warn"
            },
            detail: terminal,
            required: false,
        },
        Check {
            name: "Global hotkey",
            status: if paths.hotkey_plist.exists() {
                "ok"
            } else {
                "warn"
            },
            detail: if paths.hotkey_plist.exists() {
                "LaunchAgent installed".to_owned()
            } else {
                "optional helper not installed".to_owned()
            },
            required: false,
        },
        Check {
            name: "Key bindings",
            status: if paths.config_dir.join("keybindings.zsh").is_file() {
                "ok"
            } else {
                "warn"
            },
            detail: paths
                .config_dir
                .join("keybindings.zsh")
                .display()
                .to_string(),
            required: false,
        },
    ];
    if let Some(value) = config_value.as_ref() {
        if let Err(error) = validate_config(value) {
            checks.push(Check {
                name: "Config values",
                status: "fail",
                detail: error.to_string(),
                required: true,
            });
        } else {
            checks.push(Check {
                name: "Config values",
                status: "ok",
                detail: "valid".to_owned(),
                required: true,
            });
        }
    }

    checks
}

fn public_terminal_name(value: &str) -> Option<&'static str> {
    match value {
        "Apple_Terminal" => Some("Terminal.app"),
        "iTerm.app" => Some("iTerm2"),
        "WarpTerminal" => Some("Warp"),
        "WezTerm" => Some("WezTerm"),
        "kitty" => Some("kitty"),
        "Alacritty" => Some("Alacritty"),
        "vscode" => Some("Visual Studio Code"),
        _ => None,
    }
}

fn config_command(paths: &Paths, action: Option<ConfigAction>) -> Result<()> {
    paths.create_runtime()?;
    if !paths.config.exists() {
        atomic_write(&paths.config, DEFAULT_CONFIG, 0o600)?;
    }
    match action.unwrap_or(ConfigAction::Show) {
        ConfigAction::Show => print!("{}", fs::read_to_string(&paths.config)?),
        ConfigAction::Path => println!("{}", paths.config.display()),
        ConfigAction::Validate => {
            let value: toml::Value = toml::from_str(&fs::read_to_string(&paths.config)?)?;
            validate_config(&value)?;
            write_shell_settings(paths)?;
            println!("configuration is valid");
        }
        ConfigAction::Set { key, value } => set_config_value(paths, &key, &value)?,
        ConfigAction::Edit => {
            let editor = env::var("VISUAL")
                .or_else(|_| env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".to_owned());
            let mut pieces = shell_words::split(&editor).context("parse $VISUAL/$EDITOR")?;
            let program = pieces
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("empty editor command"))?;
            pieces.remove(0);
            let status = Command::new(program)
                .args(pieces)
                .arg(&paths.config)
                .status()
                .context("launch editor")?;
            if !status.success() {
                bail!("editor exited with {status}");
            }
            let value: toml::Value = toml::from_str(&fs::read_to_string(&paths.config)?)?;
            validate_config(&value)?;
            write_shell_settings(paths)?;
        }
        ConfigAction::SetKey => set_keychain_key(paths)?,
        ConfigAction::DeleteKey => delete_keychain_key(paths)?,
    }
    Ok(())
}

fn memory_command(paths: &Paths, action: Option<MemoryAction>) -> Result<()> {
    match action.unwrap_or(MemoryAction::Status(OutputArgs { json: false })) {
        MemoryAction::Status(output) => {
            let config = if paths.config.exists() {
                aicoach_core::Config::load_from(&paths.config)?
            } else {
                aicoach_core::Config::default()
            };
            let snapshot = aicoach_core::FailureMemorySnapshot::load(&paths.failure_memory)?;
            let metadata = fs::metadata(&paths.failure_memory).ok();
            let report = MemoryStatusReport {
                enabled: config.memory.enabled,
                path: paths.failure_memory.display().to_string(),
                exists: metadata.is_some(),
                bytes: metadata.as_ref().map_or(0, fs::Metadata::len),
                entries: snapshot.entries.len(),
                max_entries: config.memory.max_entries,
                retention_days: config.memory.retention_days,
                resolution_window_minutes: config.memory.resolution_window_minutes,
                persisted_fields: [
                    "hashed failure fingerprint",
                    "executable family",
                    "occurrence count",
                    "last-seen time",
                    "redacted successful follow-up",
                    "reusable flag",
                ],
                excluded_fields: [
                    "failed command",
                    "stdout/stderr diagnostic",
                    "terminal session identifier",
                ],
            };
            if output.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Failure Fingerprints");
                println!("  enabled:    {}", yes_no(report.enabled));
                println!("  entries:    {} / {}", report.entries, report.max_entries);
                println!("  retention:  {} days", report.retention_days);
                println!(
                    "  association: next successful command within {} minutes",
                    report.resolution_window_minutes
                );
                println!("  file:       {}", report.path);
                println!("  size:       {} bytes", report.bytes);
                println!("  never saved: failed commands, diagnostics, or session IDs");
            }
        }
        MemoryAction::List(output) => {
            let snapshot = aicoach_core::FailureMemorySnapshot::load(&paths.failure_memory)?;
            if output.json {
                println!("{}", serde_json::to_string_pretty(&snapshot)?);
            } else if snapshot.entries.is_empty() {
                println!("No failure fingerprints are retained.");
            } else {
                for entry in snapshot.entries {
                    let family =
                        aicoach_core::strip_terminal_sequences(&entry.command_family, false);
                    let follow_up =
                        aicoach_core::strip_terminal_sequences(&entry.successful_follow_up, false);
                    let seen = chrono::DateTime::from_timestamp_millis(entry.last_seen_unix_ms)
                        .map_or_else(
                            || entry.last_seen_unix_ms.to_string(),
                            |value| value.to_rfc3339(),
                        );
                    println!(
                        "{} · {} occurrences · last seen {}",
                        family, entry.occurrences, seen
                    );
                    println!("  next successful command: {follow_up}");
                    if !entry.reusable {
                        println!(
                            "  contains redaction placeholders; review only, do not reuse as-is"
                        );
                    }
                }
            }
        }
        MemoryAction::Clear => {
            let was_running = is_daemon_running(paths).0;
            if was_running {
                stop(paths)?;
            }
            remove_file_if_exists(&paths.failure_memory)?;
            if was_running {
                start(paths)?;
            }
            println!("All retained failure fingerprints were removed.");
        }
    }
    Ok(())
}

fn logs(paths: &Paths, args: &LogsArgs) -> Result<()> {
    let mut file = latest_log_file(paths)?;
    let content = fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
    let lines: Vec<_> = content.lines().collect();
    for line in lines.iter().skip(lines.len().saturating_sub(args.lines)) {
        println!("{line}");
    }
    if args.follow {
        let mut offset = content.len() as u64;
        loop {
            thread::sleep(Duration::from_millis(400));
            if let Ok(latest) = latest_log_file(paths)
                && latest != file
            {
                file = latest;
                offset = 0;
            }
            let mut handle = fs::File::open(&file)?;
            let len = handle.metadata()?.len();
            if len < offset {
                offset = 0;
            }
            std::io::Seek::seek(&mut handle, std::io::SeekFrom::Start(offset))?;
            let mut appended = String::new();
            handle.read_to_string(&mut appended)?;
            if !appended.is_empty() {
                print!("{appended}");
                std::io::stdout().flush()?;
            }
            offset = len;
        }
    }
    Ok(())
}

fn latest_log_file(paths: &Paths) -> Result<PathBuf> {
    let mut candidates = vec![
        paths.logs_dir.join("aicoachd.log"),
        paths.logs_dir.join("daemon.log"),
        paths.logs_dir.join("daemon-launchd.log"),
    ];
    if let Ok(entries) = fs::read_dir(&paths.logs_dir) {
        candidates.extend(
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name.starts_with("aicoachd.log.") || name.starts_with("aicoachd.jsonl")
                        })
                }),
        );
    }
    candidates
        .into_iter()
        .filter(|path| path.is_file())
        .max_by_key(|path| {
            path.metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH)
        })
        .ok_or_else(|| anyhow!("no daemon log found in {}", paths.logs_dir.display()))
}

fn toggle(paths: &Paths, args: &ToggleArgs) -> Result<()> {
    ensure_macos()?;
    paths.create_runtime()?;
    let config = aicoach_core::Config::load_or_create().context("load Coach window config")?;
    if !args.session.is_empty() {
        atomic_write(&paths.run_dir.join("active-session"), &args.session, 0o600)?;
    }
    if !args.tty.is_empty() {
        atomic_write(&paths.run_dir.join("active-tty"), &args.tty, 0o600)?;
    }
    let script = paths.data_dir.join("aicoach-window.js");
    if !script.exists() {
        atomic_write(&script, WINDOW_SCRIPT, 0o600)?;
    }
    let session = if args.session.is_empty() {
        fs::read_to_string(paths.run_dir.join("active-session"))
            .unwrap_or_default()
            .trim()
            .to_owned()
    } else {
        args.session.clone()
    };
    let status = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript"])
        .arg(&script)
        .arg(session)
        .arg(config.window.width.to_string())
        .arg(config.window.height.to_string())
        .arg(config.window.x.unwrap_or(120).to_string())
        .arg(config.window.y.unwrap_or(90).to_string())
        .arg(config.window.terminal.as_deref().unwrap_or("auto"))
        .status()
        .context("toggle Coach window")?;
    if !status.success() {
        bail!("window controller exited with {status}; check Automation permissions");
    }
    Ok(())
}

fn install_zshrc(paths: &Paths) -> Result<()> {
    let zshrc = paths.home.join(".zshrc");
    if fs::symlink_metadata(&zshrc).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!(
            "{} is a symbolic link; refusing to replace the link. Add the documented source line to its target manually",
            zshrc.display()
        );
    }
    let original = match fs::read_to_string(&zshrc) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "read {}; refusing to replace an unreadable or non-UTF-8 shell file",
                    zshrc.display()
                )
            });
        }
    };
    if has_complete_managed_block(&original) {
        return Ok(());
    }
    if original.contains(MANAGED_START) || original.contains(MANAGED_END) {
        bail!(
            "{} contains an incomplete AI Terminal Coach managed block; restore or remove it before installing",
            zshrc.display()
        );
    }
    if zshrc.exists() {
        let backup = paths.home.join(".zshrc.aicoach.backup");
        if !backup.exists() {
            fs::copy(&zshrc, &backup).context("backup ~/.zshrc")?;
            fs::set_permissions(
                &backup,
                fs::Permissions::from_mode(file_mode(&zshrc).unwrap_or(0o600)),
            )?;
            println!("backed up ~/.zshrc to {}", backup.display());
        }
    }
    let mut updated = original;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    use std::fmt::Write as _;
    let _ = write!(
        updated,
        "\n{MANAGED_START}\n[[ -r \"$HOME/.config/aicoach/aicoach.zsh\" ]] && source \"$HOME/.config/aicoach/aicoach.zsh\"\n{MANAGED_END}\n"
    );
    atomic_write(&zshrc, &updated, file_mode(&zshrc).unwrap_or(0o600))
}

fn has_complete_managed_block(input: &str) -> bool {
    input
        .find(MANAGED_START)
        .is_some_and(|start| input[start + MANAGED_START.len()..].contains(MANAGED_END))
}

fn remove_managed_block(input: &str) -> String {
    let Some(start) = input.find(MANAGED_START) else {
        return input.to_owned();
    };
    let Some(relative_end) = input[start..].find(MANAGED_END) else {
        return input.to_owned();
    };
    let mut end = start + relative_end + MANAGED_END.len();
    if input.as_bytes().get(end) == Some(&b'\n') {
        end += 1;
    }
    let mut result = format!("{}{}", &input[..start], &input[end..]);
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    result
}

fn set_config_value(paths: &Paths, key: &str, raw: &str) -> Result<()> {
    let mut document: toml::Value = toml::from_str(&fs::read_to_string(&paths.config)?)?;
    let pieces: Vec<_> = key.split('.').filter(|piece| !piece.is_empty()).collect();
    if pieces.is_empty() {
        bail!("configuration key cannot be empty");
    }
    let parsed = parse_toml_scalar(raw);
    let mut cursor = &mut document;
    for piece in &pieces[..pieces.len() - 1] {
        let table = cursor
            .as_table_mut()
            .ok_or_else(|| anyhow!("{piece} is not a table"))?;
        cursor = table
            .entry((*piece).to_owned())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    }
    cursor
        .as_table_mut()
        .ok_or_else(|| anyhow!("parent is not a table"))?
        .insert(pieces[pieces.len() - 1].to_owned(), parsed);
    validate_config(&document)?;
    let encoded = toml::to_string_pretty(&document)?;
    atomic_write(&paths.config, &encoded, 0o600)?;
    write_shell_settings(paths)?;
    println!("updated {key}");
    Ok(())
}

fn write_shell_settings(paths: &Paths) -> Result<()> {
    let config = aicoach_core::Config::load_from(&paths.config)
        .with_context(|| format!("load {}", paths.config.display()))?;
    let completion = zsh_ansi_quote(&keybinding_sequence(&config.keybindings.completion)?);
    let chat = zsh_ansi_quote(&keybinding_sequence(&config.keybindings.chat)?);
    let risk_lens = zsh_ansi_quote(&keybinding_sequence(&config.keybindings.risk_lens)?);
    let toggle = zsh_ansi_quote(&keybinding_sequence(&config.keybindings.toggle_coach)?);
    let contents = format!(
        "# Generated by aicoach; edit config.toml, then run `aicoach config validate`.\n\
typeset -g AICOACH_CONFIG_COMPLETION_KEY=$'{completion}'\n\
typeset -g AICOACH_CONFIG_CHAT_KEY=$'{chat}'\n\
typeset -g AICOACH_CONFIG_RISK_LENS_KEY=$'{risk_lens}'\n\
typeset -g AICOACH_CONFIG_TOGGLE_KEY=$'{toggle}'\n\
typeset -g AICOACH_CONFIG_LANGUAGE='{}'\n\
typeset -gi AICOACH_CONFIG_SAFETY_ENABLED={}\n\
typeset -gi AICOACH_CONFIG_INLINE_HINT={}\n",
        config.coach.language,
        u8::from(config.safety.enabled),
        u8::from(config.coach.inline_hint),
    );
    atomic_write(&paths.config_dir.join("keybindings.zsh"), &contents, 0o600)?;
    atomic_write(
        &paths.config_dir.join("keybindings.version"),
        &format!("{}\n", monotonic_suffix()),
        0o600,
    )
}

fn keybinding_sequence(specification: &str) -> Result<Vec<u8>> {
    let lower = specification.to_ascii_lowercase();
    let mapped = match lower.as_str() {
        "option+tab" => return Ok(vec![0x1b, b'\t']),
        "option+/" | "option+slash" => return Ok(vec![0x1b, b'/']),
        "option+r" => return Ok(vec![0x1b, b'r']),
        "option+space" => return Ok(vec![0x1b, b' ']),
        _ => specification,
    };
    let mut output = Vec::new();
    let bytes = mapped.as_bytes();
    let mut index = 0;
    if bytes.starts_with(b"^[") {
        output.push(0x1b);
        index = 2;
    } else if bytes.len() == 2 && bytes[0] == b'^' {
        output.push(if bytes[1] == b'?' {
            0x7f
        } else {
            bytes[1].to_ascii_uppercase() & 0x1f
        });
        return Ok(output);
    }
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 1 < bytes.len() {
            match bytes[index + 1] {
                b't' => output.push(b'\t'),
                b'e' => output.push(0x1b),
                b'r' => output.push(b'\r'),
                b'n' => output.push(b'\n'),
                b'\\' => output.push(b'\\'),
                other => {
                    output.push(b'\\');
                    output.push(other);
                }
            }
            index += 2;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    if output.is_empty() {
        bail!("key binding cannot be empty");
    }
    Ok(output)
}

fn zsh_ansi_quote(value: &[u8]) -> String {
    let mut output = String::new();
    for &byte in value {
        match byte {
            b'\\' => output.push_str("\\\\"),
            b'\'' => output.push_str("\\'"),
            0x20..=0x7e => output.push(char::from(byte)),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(output, "\\x{byte:02x}");
            }
        }
    }
    output
}

fn parse_toml_scalar(raw: &str) -> toml::Value {
    if let Ok(value) = raw.parse::<bool>() {
        return toml::Value::Boolean(value);
    }
    if let Ok(value) = raw.parse::<i64>() {
        return toml::Value::Integer(value);
    }
    if let Ok(value) = raw.parse::<f64>() {
        return toml::Value::Float(value);
    }
    toml::Value::String(raw.to_owned())
}

fn validate_config(value: &toml::Value) -> Result<()> {
    let encoded = toml::to_string(value)?;
    let config: aicoach_core::Config = toml::from_str(&encoded)?;
    config.validate()?;
    if config.safety.mode != aicoach_core::SafetyMode::Warn {
        bail!("this release supports safety.mode = \"warn\" only");
    }
    Ok(())
}

fn atomic_write(path: &Path, contents: &str, mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let temp = parent.join(format!(
        ".aicoach-write-{}-{}",
        std::process::id(),
        monotonic_suffix()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temp)
        .with_context(|| format!("create {}", temp.display()))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
    fs::rename(&temp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

fn secure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("secure {}", path.display()))
}

fn sibling_executable(name: &str) -> Result<PathBuf> {
    let current = env::current_exe().context("locate current executable")?;
    if let Some(candidate) = homebrew_linked_executable(&current, name) {
        return Ok(candidate);
    }
    if let Some(parent) = current.parent() {
        let candidate = parent.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    if let Some(path) = find_in_path(name) {
        return Ok(path);
    }
    bail!(
        "cannot find `{name}` next to {} or in PATH",
        current.display()
    )
}

/// Homebrew launches the CLI through a stable prefix symlink, but
/// `current_exe` resolves it to a versioned Cellar path. Persisting that path
/// in a `LaunchAgent` breaks after `brew upgrade` removes the old keg, so prefer
/// the corresponding linked executable when it exists.
fn homebrew_linked_executable(current: &Path, name: &str) -> Option<PathBuf> {
    let cellar = current
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|part| part == "Cellar"))?;
    let candidate = cellar.parent()?.join("bin").join(name);
    let metadata = fs::symlink_metadata(&candidate).ok()?;
    (metadata.file_type().is_symlink() && candidate.is_file()).then_some(candidate)
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|path| path.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn executable_path_env(cli: &Path) -> String {
    let bin = cli.parent().unwrap_or_else(|| Path::new("/usr/local/bin"));
    format!(
        "{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
        bin.display()
    )
}

fn launch_agent_plist(
    label: &str,
    program: &Path,
    args: &[&str],
    run_at_load: bool,
    keep_alive: bool,
    log: &Path,
    path_env: &str,
) -> String {
    let arguments = std::iter::once(program.display().to_string())
        .chain(args.iter().map(|s| (*s).to_owned()))
        .map(|arg| format!("      <string>{}</string>", xml_escape(&arg)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{arguments}
  </array>
  <key>RunAtLoad</key><{run}/>
  <key>KeepAlive</key><{keep}/>
  <key>ProcessType</key><string>Interactive</string>
  <key>EnvironmentVariables</key>
  <dict><key>PATH</key><string>{path_env}</string></dict>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        keep = if keep_alive { "true" } else { "false" },
        run = if run_at_load { "true" } else { "false" },
        log = xml_escape(&log.display().to_string()),
        path_env = xml_escape(path_env),
    )
}

fn bootstrap_agent(plist: &Path, label: &str, kickstart: bool) -> Result<()> {
    let domain = launch_domain()?;
    let service = format!("{domain}/{label}");
    let _ = Command::new("/bin/launchctl")
        .args(["bootout", &service])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let status = Command::new("/bin/launchctl")
        .arg("bootstrap")
        .arg(&domain)
        .arg(plist)
        .status()
        .context("launchctl bootstrap")?;
    if !status.success() {
        bail!("launchctl could not load {}", plist.display());
    }
    if kickstart {
        let status = Command::new("/bin/launchctl")
            .args(["kickstart", "-k", &service])
            .status()
            .context("launchctl kickstart")?;
        if !status.success() {
            bail!("launchctl could not start {label}");
        }
    }
    Ok(())
}

fn stop_agent(plist: &Path, label: &str) {
    if !plist.exists() {
        return;
    }
    if let Ok(domain) = launch_domain() {
        let _ = Command::new("/bin/launchctl")
            .args(["bootout", &format!("{domain}/{label}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn launch_domain() -> Result<String> {
    let output = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .context("get user id")?;
    if !output.status.success() {
        bail!("id -u failed");
    }
    Ok(format!(
        "gui/{}",
        String::from_utf8_lossy(&output.stdout).trim()
    ))
}

fn configured_key_env(paths: &Paths) -> Result<String> {
    if !paths.config.exists() {
        return Ok("AI_COACH_API_KEY".to_owned());
    }
    let config = aicoach_core::Config::load_from(&paths.config)
        .with_context(|| format!("load {}", paths.config.display()))?;
    Ok(config.ai.api_key_env)
}

fn set_keychain_key(paths: &Paths) -> Result<()> {
    ensure_macos()?;
    println!(
        "Enter the API key in the macOS Keychain prompt. It will not be written to config or shell history."
    );
    let mut child = Command::new("/usr/bin/security")
        .args([
            "add-generic-password",
            "-U",
            "-a",
            "AI_COACH_API_KEY",
            "-s",
            "com.aicoach.api-key",
            "-l",
            "AI Terminal Coach API Key",
            "-w",
        ])
        .spawn()
        .context("open macOS Keychain credential prompt")?;
    let status = child.wait()?;
    if !status.success() {
        bail!("Keychain did not save the credential");
    }
    write_keychain_wrapper(paths)?;
    println!("API credential saved in macOS Keychain");
    Ok(())
}

fn keychain_key_exists() -> bool {
    Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-a",
            "AI_COACH_API_KEY",
            "-s",
            "com.aicoach.api-key",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn delete_keychain_key(paths: &Paths) -> Result<()> {
    ensure_macos()?;
    let status = Command::new("/usr/bin/security")
        .args([
            "delete-generic-password",
            "-a",
            "AI_COACH_API_KEY",
            "-s",
            "com.aicoach.api-key",
        ])
        .status()
        .context("remove Keychain credential")?;
    if !status.success() {
        bail!("no matching Keychain credential was removed");
    }
    remove_file_if_exists(&paths.data_dir.join("aicoachd-keychain"))?;
    write_direct_daemon_plist(paths)?;
    if is_daemon_running(paths).0 {
        stop(paths)?;
        thread::sleep(Duration::from_millis(250));
        start(paths)?;
    }
    println!("API credential removed from macOS Keychain; daemon credential state refreshed");
    Ok(())
}

fn write_direct_daemon_plist(paths: &Paths) -> Result<()> {
    let daemon = sibling_executable("aicoachd")?;
    let cli = sibling_executable("aicoach")?;
    let plist = launch_agent_plist(
        DAEMON_LABEL,
        &daemon,
        &[],
        false,
        false,
        &paths.logs_dir.join("daemon-launchd.log"),
        &executable_path_env(&cli),
    );
    atomic_write(&paths.daemon_plist, &plist, 0o600)
}

fn write_keychain_wrapper(paths: &Paths) -> Result<()> {
    let daemon = sibling_executable("aicoachd")?;
    let key_env = configured_key_env(paths)?;
    let wrapper = format!(
        "#!/bin/zsh\nset -euo pipefail\nsecret=$(/usr/bin/security find-generic-password -a AI_COACH_API_KEY -s com.aicoach.api-key -w)\nexport {key_env}=\"$secret\"\nunset secret\nexec {} \"$@\"\n",
        shell_single_quote(&daemon.display().to_string())
    );
    let path = paths.data_dir.join("aicoachd-keychain");
    atomic_write(&path, &wrapper, 0o700)?;
    let cli = sibling_executable("aicoach")?;
    let plist = launch_agent_plist(
        DAEMON_LABEL,
        &path,
        &[],
        false,
        false,
        &paths.logs_dir.join("daemon-launchd.log"),
        &executable_path_env(&cli),
    );
    atomic_write(&paths.daemon_plist, &plist, 0o600)
}

fn request_shutdown(socket: &Path) -> Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_write_timeout(Some(Duration::from_millis(200)))?;
    let request = aicoach_ipc::Request::new(
        None,
        aicoach_ipc::RequestBody::Shutdown(aicoach_ipc::ShutdownParams {
            reason: Some("CLI stop".to_owned()),
        }),
    );
    writeln!(
        stream,
        "{}",
        serde_json::to_string(&aicoach_ipc::Message::from(request))?
    )?;
    Ok(())
}

fn ping_socket(socket: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
    let request = aicoach_ipc::Request::new(None, aicoach_ipc::RequestBody::Ping);
    let Ok(line) = serde_json::to_string(&aicoach_ipc::Message::from(request)) else {
        return false;
    };
    if writeln!(stream, "{line}").is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return false;
    }
    matches!(
        serde_json::from_str::<aicoach_ipc::Message>(&line),
        Ok(aicoach_ipc::Message::Response {
            response: aicoach_ipc::Response {
                outcome: aicoach_ipc::ResponseOutcome::Ok {
                    result: aicoach_ipc::ResponseResult::Pong { .. }
                },
                ..
            }
        })
    )
}

fn is_daemon_running(paths: &Paths) -> (bool, Option<u32>) {
    let pid = fs::read_to_string(&paths.pid)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok());
    let verified_pid = pid.filter(|pid| process_is_aicoachd(*pid));
    (
        ping_socket(&paths.socket) || verified_pid.is_some(),
        verified_pid,
    )
}

fn process_is_aicoachd(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let output = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return false;
    };
    output.status.success()
        && executable_name(&output.stdout).is_some_and(|name| name == "aicoachd")
}

fn executable_name(command: &[u8]) -> Option<&str> {
    let command = std::str::from_utf8(command).ok()?.trim();
    Path::new(command).file_name()?.to_str()
}

fn ensure_macos() -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("AI Terminal Coach currently supports macOS only");
    }
    Ok(())
}

fn validate_purge_target(path: &Path, expected_name: &str) -> Result<()> {
    if path.file_name().and_then(|value| value.to_str()) != Some(expected_name)
        || path.parent().is_none()
    {
        bail!("refusing unsafe purge target {}", path.display());
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

fn remove_dir_if_empty(path: &Path) {
    let _ = fs::remove_dir(path);
}
fn file_mode(path: &Path) -> Option<u32> {
    fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions().mode() & 0o777)
}
fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
fn monotonic_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_block_is_idempotent_and_removable() {
        let original = "export PATH=/bin\n";
        let installed = format!("{original}{MANAGED_START}\nsource x\n{MANAGED_END}\n");
        assert!(has_complete_managed_block(&installed));
        assert!(!has_complete_managed_block(MANAGED_START));
        assert_eq!(remove_managed_block(&installed), original);
        assert_eq!(remove_managed_block(original), original);
    }

    #[test]
    fn config_template_is_valid_and_secret_free() {
        let value: toml::Value = toml::from_str(DEFAULT_CONFIG).expect("valid TOML");
        validate_config(&value).expect("valid config");
        assert!(!DEFAULT_CONFIG.contains("sk-"));
        assert_eq!(
            value["ai"]["api_key_env"].as_str(),
            Some("AI_COACH_API_KEY")
        );
    }

    #[test]
    fn plist_escapes_paths() {
        let plist = launch_agent_plist(
            "x&y",
            Path::new("/tmp/a&b"),
            &[],
            true,
            true,
            Path::new("/tmp/log"),
            "/bin",
        );
        assert!(plist.contains("x&amp;y"));
        assert!(plist.contains("/tmp/a&amp;b"));
        assert!(plist.contains("<true/>"));
    }

    #[test]
    fn dotted_config_update_parser_preserves_types() {
        assert_eq!(parse_toml_scalar("true"), toml::Value::Boolean(true));
        assert_eq!(parse_toml_scalar("2500"), toml::Value::Integer(2500));
        assert_eq!(
            parse_toml_scalar("model"),
            toml::Value::String("model".to_owned())
        );
    }

    #[test]
    fn purge_target_is_narrow() {
        assert!(validate_purge_target(Path::new("/Users/test/.aicoach"), ".aicoach").is_ok());
        assert!(validate_purge_target(Path::new("/Users/test"), ".aicoach").is_err());
        assert!(validate_purge_target(Path::new("/"), ".aicoach").is_err());
    }

    #[test]
    fn daemon_identity_uses_the_executable_name() {
        assert_eq!(
            executable_name(b"/opt/aicoach/bin/aicoachd\n"),
            Some("aicoachd")
        );
        assert_eq!(executable_name(b"/bin/sleep\n"), Some("sleep"));
        assert_eq!(executable_name(&[0xff]), None);
    }

    #[test]
    fn memory_commands_have_explicit_read_and_delete_modes() {
        let list = Cli::try_parse_from(["aicoach", "memory", "list", "--json"]).unwrap();
        assert!(matches!(
            list.command,
            Commands::Memory(MemoryArgs {
                action: Some(MemoryAction::List(OutputArgs { json: true }))
            })
        ));
        let clear = Cli::try_parse_from(["aicoach", "memory", "clear"]).unwrap();
        assert!(matches!(
            clear.command,
            Commands::Memory(MemoryArgs {
                action: Some(MemoryAction::Clear)
            })
        ));
    }

    #[test]
    fn checkpoint_commands_parse_names_resolutions_and_status_output() {
        let start = Cli::try_parse_from([
            "aicoach",
            "checkpoint",
            "--session",
            "00000000-0000-4000-8000-000000000001",
            "start",
            "Intel build regression",
        ])
        .unwrap();
        assert!(matches!(
            start.command,
            Commands::Checkpoint(CheckpointArgs {
                action: Some(CheckpointAction::Start { ref name }),
                ..
            }) if name == "Intel build regression"
        ));
        let resolve = Cli::try_parse_from([
            "aicoach",
            "checkpoint",
            "resolve",
            "Pinned the SDK and reran tests",
        ])
        .unwrap();
        assert!(matches!(
            resolve.command,
            Commands::Checkpoint(CheckpointArgs {
                action: Some(CheckpointAction::Resolve {
                    resolution: Some(ref resolution)
                }),
                ..
            }) if resolution == "Pinned the SDK and reran tests"
        ));
        let status = Cli::try_parse_from(["aicoach", "checkpoint", "status", "--json"]).unwrap();
        assert!(matches!(
            status.command,
            Commands::Checkpoint(CheckpointArgs {
                action: Some(CheckpointAction::Status(OutputArgs { json: true })),
                ..
            })
        ));
    }

    #[test]
    fn data_commands_require_an_explicit_clear_scope() {
        let status = Cli::try_parse_from(["aicoach", "data", "status", "--json"]).unwrap();
        assert!(matches!(
            status.command,
            Commands::Data(DataArgs {
                action: Some(DataAction::Status(OutputArgs { json: true }))
            })
        ));
        let clear = Cli::try_parse_from([
            "aicoach",
            "data",
            "clear",
            "session",
            "--session",
            "00000000-0000-4000-8000-000000000001",
        ])
        .unwrap();
        assert!(matches!(
            clear.command,
            Commands::Data(DataArgs {
                action: Some(DataAction::Clear(DataClearArgs {
                    scope: DataScope::Session,
                    ..
                }))
            })
        ));
        assert!(Cli::try_parse_from(["aicoach", "data", "clear"]).is_err());
    }

    #[test]
    fn support_reports_have_explicit_copy_and_output_destinations() {
        let report =
            Cli::try_parse_from(["aicoach", "support", "--copy", "--output", "diagnostics.md"])
                .unwrap();
        assert!(matches!(
            report.command,
            Commands::Support(support::SupportArgs {
                copy: true,
                output: Some(ref output),
            }) if output == Path::new("diagnostics.md")
        ));
    }

    #[test]
    fn memory_clear_recovers_even_when_config_and_memory_are_malformed() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(directory.path());
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(&paths.config, "not valid toml").unwrap();
        fs::write(&paths.failure_memory, "not valid json").unwrap();
        memory_command(&paths, Some(MemoryAction::Clear)).unwrap();
        assert!(!paths.failure_memory.exists());
        assert!(paths.config.exists());
    }

    #[test]
    fn homebrew_launch_agents_use_the_upgrade_stable_prefix_link() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let prefix = directory.path();
        let cellar_bin = prefix.join("Cellar/aicoach/0.1.0/bin");
        let linked_bin = prefix.join("bin");
        fs::create_dir_all(&cellar_bin).unwrap();
        fs::create_dir_all(&linked_bin).unwrap();
        fs::write(cellar_bin.join("aicoachd"), "binary").unwrap();
        symlink(cellar_bin.join("aicoachd"), linked_bin.join("aicoachd")).unwrap();

        assert_eq!(
            homebrew_linked_executable(&cellar_bin.join("aicoach"), "aicoachd"),
            Some(linked_bin.join("aicoachd"))
        );
        assert_eq!(
            homebrew_linked_executable(&cellar_bin.join("aicoach"), "missing"),
            None
        );
    }

    #[test]
    fn keybinding_notation_maps_to_zle_sequences_without_shell_injection() {
        assert_eq!(keybinding_sequence("^[\\t").unwrap(), vec![0x1b, b'\t']);
        assert_eq!(
            keybinding_sequence("Option+Space").unwrap(),
            vec![0x1b, b' ']
        );
        assert_eq!(keybinding_sequence("Option+R").unwrap(), vec![0x1b, b'r']);
        assert_eq!(keybinding_sequence("^G").unwrap(), vec![0x07]);
        assert_eq!(zsh_ansi_quote(&[0x1b, b'\t']), "\\x1b\\x09");
        assert_eq!(zsh_ansi_quote(b"'$(touch /tmp/no)"), "\\'$(touch /tmp/no)");
    }

    #[test]
    fn install_refuses_to_replace_a_non_utf8_zshrc() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path();
        fs::write(home.join(".zshrc"), [0xff, b'\n']).unwrap();
        let paths = Paths::from_home(home);
        assert!(install_zshrc(&paths).is_err());
        assert_eq!(fs::read(home.join(".zshrc")).unwrap(), [0xff, b'\n']);
    }

    #[test]
    fn install_refuses_to_replace_a_zshrc_symbolic_link() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let home = directory.path();
        let target = home.join("actual-zshrc");
        fs::write(&target, "export TEST=1\n").unwrap();
        symlink(&target, home.join(".zshrc")).unwrap();
        let paths = Paths::from_home(home);
        assert!(install_zshrc(&paths).is_err());
        assert!(
            fs::symlink_metadata(home.join(".zshrc"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "export TEST=1\n");
    }
}

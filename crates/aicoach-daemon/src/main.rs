use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use aicoach_ai::{AiModels, AiTimeouts, NoopAiProvider, OpenAiCompatibleProvider, OpenAiConfig};
use aicoach_core::{Config, PrivacyRedactor, ProductPaths};
use aicoach_daemon::{Daemon, DaemonOptions, RuntimeFiles, SessionLimits};
use anyhow::{Context, Result};
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "aicoachd",
    version,
    about = "AI Terminal Coach background service"
)]
struct Arguments {
    /// Override the Unix domain socket path.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Override the TOML configuration path.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Remain attached to the invoking terminal (currently the default).
    #[arg(long)]
    foreground: bool,
    /// Accept one connection, useful for deterministic integration tests.
    #[arg(long)]
    once: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let paths = ProductPaths::discover().context("could not resolve product paths")?;
    paths
        .ensure_directories()
        .context("could not create product directories")?;
    let logs_dir = paths.logs_dir.clone();
    let _log_guard = init_logging(&paths.logs_dir);
    let config = match arguments.config.as_deref() {
        Some(path) => Config::load_from(path)
            .with_context(|| format!("could not load config {}", path.display()))?,
        None => Config::load_or_create().context("could not load or create default config")?,
    };
    let socket_path = arguments.socket.unwrap_or(paths.socket_file);
    let runtime_dir = socket_path
        .parent()
        .context("socket path must have a parent directory")?
        .to_path_buf();
    let pid_path = runtime_dir.join("aicoachd.pid");
    let runtime = RuntimeFiles::acquire(socket_path.clone(), pid_path)
        .context("could not acquire daemon runtime files")?;

    let provider: Arc<dyn aicoach_ai::AiProvider> = if config.ai.provider == "disabled"
        || config.ai.provider == "none"
    {
        Arc::new(NoopAiProvider)
    } else {
        let provider_config = OpenAiConfig {
            base_url: config.ai.base_url.clone(),
            api_key_env: config.ai.api_key_env.clone(),
            models: AiModels {
                completion: config.ai.models.completion.clone(),
                analysis: config.ai.models.error_analysis.clone(),
                chat: config.ai.models.chat.clone(),
            },
            temperature: config.ai.temperature,
            timeouts: AiTimeouts {
                completion: Duration::from_millis(config.ai.timeouts_ms.completion),
                analysis: Duration::from_millis(config.ai.timeouts_ms.error_analysis),
                chat: Duration::from_millis(config.ai.timeouts_ms.chat),
            },
            max_concurrency: config.ai.max_concurrent_requests,
            ..OpenAiConfig::default()
        };
        match OpenAiCompatibleProvider::new(provider_config) {
            Ok(provider) => Arc::new(provider),
            Err(error) => {
                // The terminal must remain fully usable if credentials or the
                // network provider are unavailable. Local safety and analyzer
                // responses continue through the explicit offline provider.
                warn!(error = %error, "AI provider unavailable; daemon is running in local-only mode");
                Arc::new(NoopAiProvider)
            }
        }
    };

    let options = DaemonOptions {
        session_limits: SessionLimits {
            max_commands: config.context.max_commands,
            max_output_per_command: config.context.max_output_per_command,
            max_total_chars: config.context.max_total_chars,
            history_enabled: config.history.enabled,
            max_chat_messages: config.history.max_messages,
            ..SessionLimits::default()
        },
        capture_screen_tail: config.privacy.capture_screen_tail,
        coach_language: config.coach.language.clone(),
        auto_error_analysis: config.coach.auto_error_analysis,
        inline_hint: config.coach.inline_hint,
        safety_enabled: config.safety.enabled,
        privacy_redactor: PrivacyRedactor::from_config(&config.privacy)
            .context("could not initialize privacy redaction")?,
        server_version: env!("CARGO_PKG_VERSION").to_owned(),
        active_state_dir: Some(runtime_dir),
    };
    let daemon = Daemon::new(provider, options);
    install_log_retention(logs_dir, daemon.shutdown_token());
    install_signal_handlers(daemon.shutdown_token());
    info!(
        socket = %runtime.socket_path().display(),
        pid_file = %runtime.pid_path().display(),
        foreground = arguments.foreground,
        "starting daemon"
    );
    let result = Arc::clone(&daemon)
        .serve_path(runtime.socket_path(), arguments.once)
        .await;
    runtime.cleanup();
    result.context("daemon stopped with an error")
}

fn init_logging(logs_dir: &Path) -> tracing_appender::non_blocking::WorkerGuard {
    // Hourly rotation plus startup retention keeps logs bounded. Only IDs,
    // method names, counts, and safe provider error variants are ever traced.
    let _ = prune_logs(logs_dir);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let appender = tracing_appender::rolling::hourly(logs_dir, "aicoachd.jsonl");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let _ = tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(writer)
        .with_current_span(false)
        .with_span_list(false)
        .try_init();
    guard
}

fn prune_logs(logs_dir: &Path) -> std::io::Result<()> {
    const MAX_LOG_FILES: usize = 48;
    let mut logs = fs::read_dir(logs_dir)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_ok_and(|file_type| file_type.is_file())
                && entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("aicoachd.jsonl")
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    logs.sort_unstable();
    let remove_count = logs.len().saturating_sub(MAX_LOG_FILES - 1);
    for path in logs.into_iter().take(remove_count) {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn install_log_retention(logs_dir: PathBuf, shutdown: CancellationToken) {
    drop(tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60 * 60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                _ = interval.tick() => {
                    if let Err(error) = prune_logs(&logs_dir) {
                        warn!(error = %error, "could not enforce log retention");
                    }
                }
            }
        }
    }));
}

fn install_signal_handlers(shutdown: CancellationToken) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut terminate = match signal(SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    warn!(error = %error, "could not install SIGTERM handler");
                    let _ = tokio::signal::ctrl_c().await;
                    shutdown.cancel();
                    return;
                }
            };
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if let Err(error) = result {
                        warn!(error = %error, "Ctrl-C handler failed");
                    }
                }
                _ = terminate.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        shutdown.cancel();
    });
}

use super::{
    DataAction, DataArgs, DataClearArgs, DataScope, MANAGED_END, MANAGED_START, OutputArgs, Paths,
    atomic_write, capsule, ensure_macos, is_daemon_running, keychain_key_exists,
    remove_file_if_exists, start, stop,
};
use aicoach_ipc::{
    ClientCapabilities, ClientKind, DaemonDataResult, DataOperation, DataParams,
    DataRemovalSummary, HelloParams, IpcClient, PROTOCOL_VERSION, Request, RequestBody,
    ResponseOutcome, ResponseResult, SessionDataLimits, SessionDataSummary, SessionId,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_HISTORY_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Serialize)]
struct DataInventoryReport {
    persistent_stores: Vec<DataStoreReport>,
    keychain_credential: KeychainReport,
    daemon_memory: DaemonInventoryReport,
    installed_support_files: Vec<FileReport>,
    shell_integration: ShellIntegrationReport,
    shell_backup: FileReport,
    content_exposure: &'static str,
}

#[derive(Debug, Serialize)]
struct DataStoreReport {
    category: &'static str,
    path: String,
    exists: bool,
    bytes: u64,
    items: Option<usize>,
    item_unit: Option<&'static str>,
    groups: Option<usize>,
    group_unit: Option<&'static str>,
    retention: String,
    contains: Vec<&'static str>,
    clear_command: Option<String>,
    read_error: Option<String>,
}

#[derive(Debug, Serialize)]
struct FileReport {
    purpose: &'static str,
    path: String,
    exists: bool,
    bytes: u64,
}

#[derive(Debug, Serialize)]
struct KeychainReport {
    service: &'static str,
    account: &'static str,
    configured: bool,
    secret_read: bool,
    delete_command: &'static str,
}

#[derive(Debug, Serialize)]
struct ShellIntegrationReport {
    path: String,
    exists: bool,
    managed_block_present: bool,
    content_printed: bool,
}

#[derive(Debug, Serialize)]
struct DaemonInventoryReport {
    available: bool,
    memory_only: bool,
    sessions: Vec<SessionDataSummary>,
    source_card_cache_entries: Option<usize>,
    limits: Option<SessionDataLimits>,
    read_error: Option<String>,
    retained_categories: [&'static str; 14],
    provider_boundary: &'static str,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HistoryDocument {
    #[serde(default)]
    sessions: BTreeMap<String, Vec<Value>>,
    #[serde(default)]
    updated_at_micros: BTreeMap<String, i64>,
    #[serde(flatten)]
    additional_fields: BTreeMap<String, Value>,
}

pub(super) fn run(paths: &Paths, args: &DataArgs) -> Result<()> {
    ensure_macos()?;
    match args
        .action
        .as_ref()
        .unwrap_or(&DataAction::Status(OutputArgs { json: false }))
    {
        DataAction::Status(output) => print_inventory(paths, output.json),
        DataAction::Sessions(output) => print_sessions(paths, output.json),
        DataAction::Clear(args) => clear(paths, args),
    }
}

fn print_inventory(paths: &Paths, json: bool) -> Result<()> {
    let report = inventory(paths);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("AI Terminal Coach local data inventory");
    println!("Persistent stores (contents are not printed):");
    for store in &report.persistent_stores {
        let items = store
            .items
            .zip(store.item_unit)
            .map_or_else(String::new, |(items, unit)| format!(", {items} {unit}"));
        let groups = store
            .groups
            .zip(store.group_unit)
            .map_or_else(String::new, |(groups, unit)| format!(", {groups} {unit}"));
        println!(
            "  {:<18} {:>8} bytes{}{}  {}",
            store.category, store.bytes, items, groups, store.path
        );
        println!("    retention: {}", store.retention);
        if let Some(error) = store.read_error.as_deref() {
            println!("    unreadable metadata: {error}");
        }
    }
    println!(
        "Keychain credential: {} (secret value was not read)",
        if report.keychain_credential.configured {
            "configured"
        } else {
            "not configured"
        }
    );
    if report.daemon_memory.available {
        let sessions = &report.daemon_memory.sessions;
        println!(
            "Daemon memory: {} sessions, {} command records, {} chat messages",
            sessions.len(),
            sessions
                .iter()
                .map(|session| session.command_records)
                .sum::<usize>(),
            sessions
                .iter()
                .map(|session| session.chat_messages)
                .sum::<usize>()
        );
        println!("  use aicoach data sessions for per-session counts");
    } else {
        println!("Daemon memory: unavailable (daemon is not running or did not respond)");
    }
    println!("Installed support files:");
    for file in report
        .installed_support_files
        .iter()
        .filter(|file| file.exists)
    {
        println!("  {}: {}", file.purpose, file.path);
    }
    println!(
        "Shell integration: {} ({})",
        if report.shell_integration.managed_block_present {
            "managed block present"
        } else {
            "managed block absent"
        },
        report.shell_integration.path
    );
    if report.shell_backup.exists {
        println!(
            "Shell backup: {} (never deleted by data clear commands)",
            report.shell_backup.path
        );
    }
    println!("Clear one scope with: aicoach data clear session|history|fingerprints|logs|all");
    println!("Config and Keychain credentials are preserved by every data clear scope.");
    Ok(())
}

fn print_sessions(paths: &Paths, json: bool) -> Result<()> {
    let result = request_daemon(paths, None, DataOperation::Inventory)
        .context("read daemon memory inventory")?;
    let DaemonDataResult::Inventory {
        sessions,
        source_card_cache_entries,
        limits,
    } = result
    else {
        bail!("daemon returned a clear result for an inventory request")
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&DaemonDataResult::Inventory {
                sessions,
                source_card_cache_entries,
                limits,
            })?
        );
        return Ok(());
    }
    if sessions.is_empty() {
        println!("No terminal sessions are retained in daemon memory.");
        return Ok(());
    }
    println!(
        "SESSION                               LINK  CMD  CHAT  ENV  CP  BASE  RUN  DROP  AI  PENDING"
    );
    for session in sessions {
        println!(
            "{}  {:<4}  {:>3}  {:>4}  {:>3}  {:>2}  {:>4}  {:>3}  {:>4}  {:>2}  {:>7}",
            session.session_id,
            if session.connected { "yes" } else { "no" },
            session.command_records,
            session.chat_messages,
            session.environment_values,
            yes_no_short(session.checkpoint_present),
            yes_no_short(session.environment_baseline_present),
            session.in_flight_commands,
            session.discarded_finish_markers,
            session.active_ai_requests,
            yes_no_short(session.pending_failure),
        );
    }
    println!(
        "No command text, output, cwd, environment value, checkpoint text, or chat content is shown."
    );
    Ok(())
}

fn clear(paths: &Paths, args: &DataClearArgs) -> Result<()> {
    if args.scope != DataScope::Session && !args.session.trim().is_empty() {
        bail!("--session is valid only with the session clear scope");
    }
    match args.scope {
        DataScope::Session => clear_session(paths, &args.session),
        DataScope::History => clear_history(paths),
        DataScope::Fingerprints => clear_fingerprints(paths),
        DataScope::Logs => clear_logs(paths),
        DataScope::All => clear_all(paths),
    }
}

fn clear_session(paths: &Paths, requested: &str) -> Result<()> {
    let session_id = capsule::resolve_capsule_session(paths, requested)?;
    let result = request_daemon(paths, Some(session_id), DataOperation::ClearSession)?;
    let mut removed = expect_clear(result)?;
    removed.persisted_chat_messages = remove_history_session(&paths.history, session_id)?;
    print_removed("Session data", &removed);
    println!("The live Shell connection was preserved; the clear command itself is not retained.");
    Ok(())
}

fn clear_history(paths: &Paths) -> Result<()> {
    let mut removed = DataRemovalSummary::default();
    if is_daemon_running(paths).0 {
        removed = expect_clear(request_daemon(
            paths,
            None,
            DataOperation::ClearChatHistory,
        )?)?;
    }
    removed.persisted_chat_messages = history_counts(&paths.history).1.unwrap_or_default();
    remove_file_if_exists(&paths.history)?;
    print_removed("Chat history", &removed);
    Ok(())
}

fn clear_logs(paths: &Paths) -> Result<()> {
    let running = is_daemon_running(paths).0;
    let cleared = clear_log_files(&paths.logs_dir, running)?;
    println!("Daemon logs cleared: {cleared} files. Config and Keychain were preserved.");
    Ok(())
}

fn clear_fingerprints(paths: &Paths) -> Result<()> {
    let mut removed = DataRemovalSummary::default();
    if is_daemon_running(paths).0 {
        removed = expect_clear(request_daemon(
            paths,
            None,
            DataOperation::ClearFailureMemory,
        )?)?;
    }
    remove_file_if_exists(&paths.failure_memory)?;
    println!(
        "Failure memory cleared: {} fingerprints and {} pending links. Sessions were preserved.",
        removed.failure_fingerprints, removed.pending_failures
    );
    Ok(())
}

fn clear_all(paths: &Paths) -> Result<()> {
    let was_running = is_daemon_running(paths).0;
    let mut removed = DataRemovalSummary::default();
    if was_running {
        removed = expect_clear(request_daemon(
            paths,
            None,
            DataOperation::ClearAllTransient,
        )?)?;
    }
    removed.persisted_chat_messages = history_counts(&paths.history).1.unwrap_or_default();
    let log_files = with_daemon_stopped(paths, || {
        remove_file_if_exists(&paths.history)?;
        remove_file_if_exists(&paths.failure_memory)?;
        remove_file_if_exists(&paths.window_state)?;
        remove_file_if_exists(&paths.run_dir.join("active-session"))?;
        remove_file_if_exists(&paths.run_dir.join("active-tty"))?;
        clear_log_files(&paths.logs_dir, false)
    })?;
    print_removed("All transient daemon data", &removed);
    println!(
        "Persistent history, fingerprints, window state, runtime markers, and {log_files} log files were removed."
    );
    println!("Configuration, installed support files, Shell backup, and Keychain were preserved.");
    Ok(())
}

fn with_daemon_stopped<T>(paths: &Paths, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let was_running = is_daemon_running(paths).0;
    if was_running {
        stop(paths)?;
    }
    let result = operation();
    let restart = if was_running { start(paths) } else { Ok(()) };
    match (result, restart) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => {
            Err(error).context("local data was cleared but daemon restart failed")
        }
        (Err(error), Err(restart_error)) => {
            bail!("{error:#}; daemon restart also failed: {restart_error:#}")
        }
    }
}

fn inventory(paths: &Paths) -> DataInventoryReport {
    let (config, config_error) = match aicoach_core::Config::load_from(&paths.config) {
        Ok(config) => (config, None),
        Err(_) if !paths.config.exists() => (aicoach_core::Config::default(), None),
        Err(error) => (aicoach_core::Config::default(), Some(error.to_string())),
    };
    let (history_sessions, history_messages, history_error) = history_counts(&paths.history);
    let (fingerprints, fingerprint_error) =
        match aicoach_core::FailureMemorySnapshot::load(&paths.failure_memory) {
            Ok(snapshot) => (Some(snapshot.entries.len()), None),
            Err(error) => (None, Some(error.to_string())),
        };
    let (log_files, log_bytes, log_error) = directory_counts(&paths.logs_dir, is_log_file);
    let runtime_paths = [
        paths.socket.clone(),
        paths.pid.clone(),
        paths.run_dir.join("active-session"),
        paths.run_dir.join("active-tty"),
    ];
    let (runtime_items, runtime_bytes) = known_file_counts(&runtime_paths);
    let daemon_memory = match request_daemon(paths, None, DataOperation::Inventory) {
        Ok(DaemonDataResult::Inventory {
            sessions,
            source_card_cache_entries,
            limits,
        }) => DaemonInventoryReport {
            available: true,
            memory_only: true,
            sessions,
            source_card_cache_entries: Some(source_card_cache_entries),
            limits: Some(limits),
            read_error: None,
            retained_categories: daemon_categories(),
            provider_boundary: "checkpoint, failure-memory, environment-drift, and control metadata are never added to provider requests; command/chat context follows privacy redaction",
        },
        Ok(DaemonDataResult::Cleared { .. }) => DaemonInventoryReport {
            available: false,
            memory_only: true,
            sessions: Vec::new(),
            source_card_cache_entries: None,
            limits: None,
            read_error: Some("daemon returned an unexpected response".to_owned()),
            retained_categories: daemon_categories(),
            provider_boundary: "checkpoint, failure-memory, environment-drift, and control metadata are never added to provider requests; command/chat context follows privacy redaction",
        },
        Err(error) => DaemonInventoryReport {
            available: false,
            memory_only: true,
            sessions: Vec::new(),
            source_card_cache_entries: None,
            limits: None,
            read_error: Some(format!("{error:#}")),
            retained_categories: daemon_categories(),
            provider_boundary: "checkpoint, failure-memory, environment-drift, and control metadata are never added to provider requests; command/chat context follows privacy redaction",
        },
    };

    DataInventoryReport {
        persistent_stores: vec![
            DataStoreReport {
                category: "configuration",
                path: paths.config.display().to_string(),
                exists: paths.config.is_file(),
                bytes: file_size(&paths.config),
                items: None,
                item_unit: None,
                groups: None,
                group_unit: None,
                retention: "until edited or uninstalled with --purge".to_owned(),
                contains: vec!["settings", "provider URL", "model names", "privacy rules"],
                clear_command: Some("aicoach uninstall --purge".to_owned()),
                read_error: config_error,
            },
            DataStoreReport {
                category: "chat_history",
                path: paths.history.display().to_string(),
                exists: paths.history.is_file(),
                bytes: file_size(&paths.history),
                items: history_messages,
                item_unit: Some("messages"),
                groups: history_sessions,
                group_unit: Some("sessions"),
                retention: format!(
                    "up to {} messages per session and {} sessions; enabled={}",
                    config.history.max_messages,
                    aicoach_core::MAX_PERSISTED_HISTORY_SESSIONS,
                    config.history.enabled
                ),
                contains: vec![
                    "user messages",
                    "coach messages",
                    "system messages",
                    "session IDs",
                ],
                clear_command: Some("aicoach data clear history".to_owned()),
                read_error: history_error,
            },
            DataStoreReport {
                category: "fingerprints",
                path: paths.failure_memory.display().to_string(),
                exists: paths.failure_memory.is_file(),
                bytes: file_size(&paths.failure_memory),
                items: fingerprints,
                item_unit: Some("entries"),
                groups: None,
                group_unit: None,
                retention: format!(
                    "up to {} entries for {} days; enabled={}",
                    config.memory.max_entries, config.memory.retention_days, config.memory.enabled
                ),
                contains: vec![
                    "hashed failure fingerprint",
                    "executable family",
                    "occurrence count",
                    "last-seen time",
                    "redacted successful follow-up",
                ],
                clear_command: Some("aicoach data clear fingerprints".to_owned()),
                read_error: fingerprint_error,
            },
            DataStoreReport {
                category: "daemon_logs",
                path: paths.logs_dir.display().to_string(),
                exists: paths.logs_dir.is_dir(),
                bytes: log_bytes,
                items: log_files,
                item_unit: Some("files"),
                groups: None,
                group_unit: None,
                retention: format!(
                    "hourly rotation, at most {} daemon log files",
                    aicoach_core::MAX_DAEMON_LOG_FILES
                ),
                contains: vec![
                    "timestamps",
                    "levels",
                    "request types",
                    "IDs",
                    "safe error kinds",
                ],
                clear_command: Some("aicoach data clear logs".to_owned()),
                read_error: log_error,
            },
            file_store(
                "window_state",
                &paths.window_state,
                "until replaced or cleared",
                vec!["window placement and size"],
                Some("aicoach data clear all"),
            ),
            DataStoreReport {
                category: "runtime",
                path: paths.run_dir.display().to_string(),
                exists: paths.run_dir.is_dir(),
                bytes: runtime_bytes,
                items: Some(runtime_items),
                item_unit: Some("markers"),
                groups: None,
                group_unit: None,
                retention: "socket/PID while running; active terminal markers until replaced"
                    .to_owned(),
                contains: vec!["socket", "daemon PID", "active session ID", "active tty"],
                clear_command: Some("aicoach data clear all".to_owned()),
                read_error: None,
            },
        ],
        keychain_credential: KeychainReport {
            service: "com.aicoach.api-key",
            account: "AI_COACH_API_KEY",
            configured: keychain_key_exists(),
            secret_read: false,
            delete_command: "aicoach config delete-key",
        },
        daemon_memory,
        installed_support_files: support_files(paths)
            .into_iter()
            .map(|(purpose, path)| file_report(purpose, &path))
            .collect(),
        shell_integration: shell_integration(paths),
        shell_backup: file_report(
            "pre-install zshrc backup",
            &paths.home.join(".zshrc.aicoach.backup"),
        ),
        content_exposure: "inventory reports paths, sizes, counts, limits, and booleans only; it never prints stored content or reads the Keychain secret",
    }
}

const fn daemon_categories() -> [&'static str; 14] {
    [
        "session ID",
        "tty",
        "process ID",
        "shell",
        "terminal name",
        "current cwd",
        "allowlisted environment values",
        "bounded commands and output summaries",
        "bounded daemon chat",
        "checkpoint",
        "last-success environment baseline",
        "active request IDs and cancellation handles",
        "content-free discarded FINISH markers",
        "local manual Source Card cache",
    ]
}

fn shell_integration(paths: &Paths) -> ShellIntegrationReport {
    let path = paths.home.join(".zshrc");
    let managed_block_present = fs::read_to_string(&path)
        .is_ok_and(|contents| contents.contains(MANAGED_START) && contents.contains(MANAGED_END));
    ShellIntegrationReport {
        path: path.display().to_string(),
        exists: path.is_file(),
        managed_block_present,
        content_printed: false,
    }
}

fn request_daemon(
    paths: &Paths,
    session_id: Option<SessionId>,
    operation: DataOperation,
) -> Result<DaemonDataResult> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("create data-control IPC runtime")?;
    runtime.block_on(async {
        let client = IpcClient::connect(&paths.socket).await.with_context(|| {
            format!(
                "connect to {}; run aicoach start first",
                paths.socket.display()
            )
        })?;
        let timeout = Duration::from_secs(2);
        let hello = client
            .send_timeout(
                Request::new(
                    None,
                    RequestBody::Hello(HelloParams {
                        protocol_version: PROTOCOL_VERSION,
                        client_name: "aicoach-data".to_owned(),
                        client_version: env!("CARGO_PKG_VERSION").to_owned(),
                        client_kind: ClientKind::Cli,
                        capabilities: ClientCapabilities {
                            push_events: false,
                            streaming: false,
                            insert_buffer: false,
                            shell_line_protocol: false,
                        },
                    }),
                ),
                timeout,
            )
            .await
            .context("handshake with daemon")?;
        match hello.outcome {
            ResponseOutcome::Ok {
                result:
                    ResponseResult::Hello {
                        protocol_version, ..
                    },
            } if protocol_version == PROTOCOL_VERSION => {}
            ResponseOutcome::Error { error } => {
                bail!("daemon rejected handshake: {}", error.message)
            }
            other @ ResponseOutcome::Ok { .. } => {
                bail!("unexpected daemon handshake response: {other:?}")
            }
        }
        let response = client
            .send_timeout(
                Request::new(
                    session_id,
                    RequestBody::Data(DataParams {
                        operation,
                        exclude_active_command: true,
                    }),
                ),
                timeout,
            )
            .await
            .context("request daemon data operation")?;
        client.close().await.ok();
        match response.outcome {
            ResponseOutcome::Ok {
                result: ResponseResult::Data(result),
            } => Ok(*result),
            ResponseOutcome::Error { error } => bail!("data operation failed: {}", error.message),
            other @ ResponseOutcome::Ok { .. } => {
                bail!("unexpected daemon data response: {other:?}")
            }
        }
    })
}

fn expect_clear(result: DaemonDataResult) -> Result<DataRemovalSummary> {
    match result {
        DaemonDataResult::Cleared { removed, .. } => Ok(removed),
        DaemonDataResult::Inventory { .. } => {
            bail!("daemon returned an inventory for a clear request")
        }
    }
}

fn history_counts(path: &Path) -> (Option<usize>, Option<usize>, Option<String>) {
    match load_history(path) {
        Ok(Some(history)) => (
            Some(history.sessions.len()),
            Some(history.sessions.values().map(Vec::len).sum()),
            None,
        ),
        Ok(None) => (Some(0), Some(0), None),
        Err(error) => (None, None, Some(error.to_string())),
    }
}

fn remove_history_session(path: &Path, session_id: SessionId) -> Result<usize> {
    let Some(mut history) = load_history(path)? else {
        return Ok(0);
    };
    let key = session_id.to_string();
    let removed = history
        .sessions
        .remove(&key)
        .map_or(0, |messages| messages.len());
    history.updated_at_micros.remove(&key);
    if history.sessions.is_empty() && history.additional_fields.is_empty() {
        remove_file_if_exists(path)?;
        return Ok(removed);
    }
    atomic_write(path, &serde_json::to_string_pretty(&history)?, 0o600)?;
    Ok(removed)
}

fn load_history(path: &Path) -> Result<Option<HistoryDocument>> {
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() > MAX_HISTORY_BYTES) {
        bail!(
            "history file at {} exceeds the {} byte inspection limit",
            path.display(),
            MAX_HISTORY_BYTES
        );
    }
    match fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents)
            .map(Some)
            .with_context(|| format!("parse {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn file_store(
    category: &'static str,
    path: &Path,
    retention: &'static str,
    contains: Vec<&'static str>,
    clear_command: Option<&'static str>,
) -> DataStoreReport {
    DataStoreReport {
        category,
        path: path.display().to_string(),
        exists: path.is_file(),
        bytes: file_size(path),
        items: None,
        item_unit: None,
        groups: None,
        group_unit: None,
        retention: retention.to_owned(),
        contains,
        clear_command: clear_command.map(ToOwned::to_owned),
        read_error: None,
    }
}

fn support_files(paths: &Paths) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("Zsh integration", paths.config_dir.join("aicoach.zsh")),
        (
            "shortcut settings",
            paths.config_dir.join("keybindings.zsh"),
        ),
        (
            "shortcut schema version",
            paths.config_dir.join("keybindings.version"),
        ),
        (
            "window controller",
            paths.data_dir.join("aicoach-window.js"),
        ),
        ("window hider", paths.data_dir.join("aicoach-hide.js")),
        (
            "Keychain launcher",
            paths.data_dir.join("aicoachd-keychain"),
        ),
        ("daemon LaunchAgent", paths.daemon_plist.clone()),
        ("hotkey LaunchAgent", paths.hotkey_plist.clone()),
    ]
}

fn file_report(purpose: &'static str, path: &Path) -> FileReport {
    FileReport {
        purpose,
        path: path.display().to_string(),
        exists: path.is_file(),
        bytes: file_size(path),
    }
}

fn file_size(path: &Path) -> u64 {
    fs::symlink_metadata(path).map_or(0, |metadata| metadata.len())
}

fn known_file_counts(paths: &[PathBuf]) -> (usize, u64) {
    paths
        .iter()
        .filter(|path| path.exists())
        .fold((0, 0), |(items, bytes), path| {
            (items + 1, bytes + file_size(path))
        })
}

fn directory_counts(
    directory: &Path,
    include: fn(&Path) -> bool,
) -> (Option<usize>, u64, Option<String>) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (Some(0), 0, None),
        Err(error) => return (None, 0, Some(error.to_string())),
    };
    let mut count = 0;
    let mut bytes = 0_u64;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => return (None, bytes, Some(error.to_string())),
        };
        let path = entry.path();
        if include(&path) && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            count += 1;
            bytes = bytes.saturating_add(file_size(&path));
        }
    }
    (Some(count), bytes, None)
}

fn is_log_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(name, "aicoachd.jsonl" | "aicoachd.log")
                || name.starts_with("aicoachd.jsonl.")
                || name.starts_with("aicoachd.log.")
                || matches!(name, "daemon.log" | "daemon-launchd.log" | "hotkey.log")
        })
}

fn clear_log_files(directory: &Path, keep_open_files: bool) -> Result<usize> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).with_context(|| format!("read {}", directory.display())),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if is_log_file(&path) && entry.file_type()?.is_file() {
            if keep_open_files {
                fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&path)
                    .with_context(|| format!("truncate {}", path.display()))?;
            } else {
                fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
            }
            removed += 1;
        }
    }
    Ok(removed)
}

fn print_removed(label: &str, removed: &DataRemovalSummary) {
    println!("{label} cleared:");
    println!(
        "  {} sessions, {} commands, {} memory chat messages, {} persisted chat messages, {} environment values",
        removed.sessions_affected,
        removed.command_records,
        removed.chat_messages,
        removed.persisted_chat_messages,
        removed.environment_values
    );
    println!(
        "  {} checkpoints, {} baselines, {} in-flight commands, {} AI requests",
        removed.checkpoints,
        removed.environment_baselines,
        removed.in_flight_commands,
        removed.active_ai_requests
    );
    println!(
        "  {} fingerprints, {} pending failures, {} local source-cache entries",
        removed.failure_fingerprints, removed.pending_failures, removed.source_card_cache_entries
    );
}

const fn yes_no_short(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn session_history_clear_preserves_other_sessions_and_unknown_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        let removed = SessionId::new();
        let retained = SessionId::new();
        let mut history = HistoryDocument::default();
        history.sessions.insert(
            removed.to_string(),
            vec![serde_json::json!({"speaker":"user","content":"private"})],
        );
        history.sessions.insert(
            retained.to_string(),
            vec![serde_json::json!({"speaker":"coach","content":"safe"})],
        );
        history.updated_at_micros.insert(removed.to_string(), 1);
        history.updated_at_micros.insert(retained.to_string(), 2);
        history
            .additional_fields
            .insert("future_field".to_owned(), serde_json::json!({"keep":true}));
        fs::write(&path, serde_json::to_vec(&history).unwrap()).unwrap();

        remove_history_session(&path, removed).unwrap();

        let saved = load_history(&path).unwrap().unwrap();
        assert!(!saved.sessions.contains_key(&removed.to_string()));
        assert!(saved.sessions.contains_key(&retained.to_string()));
        assert_eq!(
            saved.additional_fields.get("future_field"),
            Some(&serde_json::json!({"keep":true}))
        );
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn malformed_history_is_preserved_during_session_clear() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        fs::write(&path, "{broken").unwrap();
        assert!(remove_history_session(&path, SessionId::new()).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "{broken");
    }

    #[test]
    fn oversized_history_is_rejected_before_reading_or_rewriting() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.json");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_HISTORY_BYTES + 1).unwrap();

        assert!(remove_history_session(&path, SessionId::new()).is_err());
        assert_eq!(fs::metadata(path).unwrap().len(), MAX_HISTORY_BYTES + 1);
    }

    #[test]
    fn log_clear_is_narrow_and_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let logs = directory.path().join("logs");
        fs::create_dir(&logs).unwrap();
        fs::write(logs.join("aicoachd.jsonl.2026-09-05-10"), "log").unwrap();
        fs::write(logs.join("aicoachd.jsonl-private"), "keep").unwrap();
        fs::write(logs.join("unrelated.txt"), "keep").unwrap();
        let outside = directory.path().join("outside");
        fs::write(&outside, "keep").unwrap();
        symlink(&outside, logs.join("aicoachd.log.link")).unwrap();

        assert_eq!(clear_log_files(&logs, false).unwrap(), 1);
        assert!(logs.join("aicoachd.jsonl-private").exists());
        assert!(logs.join("unrelated.txt").exists());
        assert!(logs.join("aicoachd.log.link").exists());
        assert_eq!(fs::read_to_string(outside).unwrap(), "keep");
    }

    #[test]
    fn running_daemon_logs_are_truncated_without_unlinking_open_files() {
        let directory = tempfile::tempdir().unwrap();
        let logs = directory.path().join("logs");
        fs::create_dir(&logs).unwrap();
        let current = logs.join("aicoachd.jsonl.2026-09-05-10");
        fs::write(&current, "private log line").unwrap();

        assert_eq!(clear_log_files(&logs, true).unwrap(), 1);
        assert!(current.exists());
        assert_eq!(file_size(&current), 0);
    }

    #[test]
    fn inventory_never_serializes_chat_contents() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(directory.path());
        fs::create_dir_all(&paths.state_dir).unwrap();
        let mut history = HistoryDocument::default();
        history.sessions.insert(
            SessionId::new().to_string(),
            vec![serde_json::json!({
                "speaker":"user",
                "content":"private-history-payload"
            })],
        );
        fs::write(&paths.history, serde_json::to_vec(&history).unwrap()).unwrap();

        let encoded = serde_json::to_string(&inventory(&paths)).unwrap();
        assert!(encoded.contains(r#""items":1"#));
        assert!(!encoded.contains("private-history-payload"));
    }

    #[test]
    fn all_scope_removes_only_data_and_preserves_config_support_and_backup() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(directory.path());
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::create_dir_all(&paths.data_dir).unwrap();
        fs::create_dir_all(&paths.logs_dir).unwrap();
        fs::create_dir_all(&paths.run_dir).unwrap();
        fs::write(&paths.config, "[ai]\nprovider = \"disabled\"\n").unwrap();
        let support = paths.config_dir.join("aicoach.zsh");
        fs::write(&support, "support").unwrap();
        let backup = paths.home.join(".zshrc.aicoach.backup");
        fs::write(&backup, "backup").unwrap();
        fs::write(&paths.history, "{}").unwrap();
        fs::write(&paths.failure_memory, "{}").unwrap();
        fs::write(&paths.window_state, "{}").unwrap();
        fs::write(
            paths.run_dir.join("active-session"),
            SessionId::new().to_string(),
        )
        .unwrap();
        fs::write(paths.run_dir.join("active-tty"), "/dev/ttys001").unwrap();
        fs::write(
            paths.logs_dir.join("aicoachd.jsonl.2026-09-05-10"),
            "private",
        )
        .unwrap();
        let unrelated = paths.logs_dir.join("unrelated.txt");
        fs::write(&unrelated, "keep").unwrap();

        clear_all(&paths).unwrap();

        assert!(paths.config.exists());
        assert!(support.exists());
        assert!(backup.exists());
        assert!(unrelated.exists());
        assert!(!paths.history.exists());
        assert!(!paths.failure_memory.exists());
        assert!(!paths.window_state.exists());
        assert!(!paths.run_dir.join("active-session").exists());
        assert!(!paths.run_dir.join("active-tty").exists());
        assert!(!paths.logs_dir.join("aicoachd.jsonl.2026-09-05-10").exists());
    }
}

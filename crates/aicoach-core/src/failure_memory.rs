//! Bounded, local-only memory for recurring command failures.
//!
//! The persisted format deliberately excludes failed commands and diagnostic
//! output. Those values are normalized and hashed in memory; only a short,
//! always-redacted successful follow-up command is written to disk.

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{PrivacyRedactor, strip_terminal_sequences};

const SCHEMA_VERSION: u32 = 1;
const MAX_STORED_COMMAND_CHARS: usize = 320;
const MAX_FINGERPRINT_OUTPUT_CHARS: usize = 1_024;
const MAX_COMMAND_FAMILY_CHARS: usize = 64;
const MAX_MEMORY_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SCHEMA_ENTRIES: usize = 4_096;

#[derive(Debug, Clone)]
pub struct FailureMemoryOptions {
    pub path: PathBuf,
    pub home_dir: PathBuf,
    pub max_entries: usize,
    pub retention: Duration,
    pub resolution_window: Duration,
    /// Must be enabled. The daemon constructs this from privacy settings while
    /// forcing redaction on, so user extra patterns continue to apply.
    pub redactor: PrivacyRedactor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailureFingerprint {
    /// SHA-256 of a normalized command/error shape, never the raw values.
    pub fingerprint: String,
    /// Executable family only, such as `git` or `cargo`.
    pub command_family: String,
    /// Number of matching failures observed after a useful follow-up existed.
    pub occurrences: u32,
    pub last_seen_unix_ms: i64,
    /// Always passed through built-in secret redaction before persistence.
    pub successful_follow_up: String,
    /// False when the displayed command contains a redaction placeholder.
    pub reusable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailureMemorySnapshot {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<FailureFingerprint>,
}

impl Default for FailureMemorySnapshot {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }
}

impl FailureMemorySnapshot {
    /// Read a snapshot without modifying it. A missing file is an empty store.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable, malformed, or unsupported data.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, FailureMemoryError> {
        let path = path.as_ref();
        if let Ok(metadata) = fs::metadata(path)
            && metadata.len() > MAX_MEMORY_FILE_BYTES
        {
            return Err(FailureMemoryError::TooLarge {
                path: path.to_path_buf(),
                bytes: metadata.len(),
            });
        }
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(FailureMemoryError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let snapshot: Self =
            serde_json::from_str(&source).map_err(|source| FailureMemoryError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        if snapshot.version != SCHEMA_VERSION {
            return Err(FailureMemoryError::UnsupportedVersion {
                path: path.to_path_buf(),
                version: snapshot.version,
            });
        }
        validate_snapshot(path, &snapshot)?;
        Ok(snapshot)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureMemoryRecall {
    pub command_family: String,
    pub occurrences: u32,
    pub successful_follow_up: String,
    pub reusable: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FailureMemoryObservation {
    pub recall: Option<FailureMemoryRecall>,
    /// True when the persisted snapshot needs an atomic refresh.
    pub changed: bool,
}

#[derive(Debug, Clone)]
struct PendingFailure {
    fingerprint: String,
    command_shape: String,
    command_family: String,
    observed_at_unix_ms: i64,
}

#[derive(Debug)]
pub struct FailureMemory {
    options: FailureMemoryOptions,
    snapshot: FailureMemorySnapshot,
    pending: HashMap<Uuid, PendingFailure>,
    forced_redactor: PrivacyRedactor,
}

impl FailureMemory {
    /// Load a store. Malformed existing data is reported and never overwritten.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot cannot be loaded or decoded.
    pub fn load(options: FailureMemoryOptions) -> Result<Self, FailureMemoryError> {
        let snapshot = FailureMemorySnapshot::load(&options.path)?;
        let forced_redactor = if options.redactor.is_enabled() {
            options.redactor.clone()
        } else {
            PrivacyRedactor::default()
        };
        let mut memory = Self {
            options,
            snapshot,
            pending: HashMap::new(),
            // Local memory must stay scrubbed even when provider redaction is
            // explicitly disabled in user configuration.
            forced_redactor,
        };
        let sanitized = memory.sanitize_loaded_entries();
        if memory.prune(Utc::now().timestamp_millis()) || sanitized {
            memory.persist()?;
        }
        Ok(memory)
    }

    pub fn entries(&self) -> &[FailureFingerprint] {
        &self.snapshot.entries
    }

    /// Observe a completed command and update only local state.
    /// Call [`Self::persist`] when the returned observation is changed.
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &mut self,
        session_id: Uuid,
        command: &str,
        exit_code: i32,
        stdout: Option<&str>,
        stderr: Option<&str>,
        now_unix_ms: i64,
    ) -> FailureMemoryObservation {
        let window_ms = duration_millis_i64(self.options.resolution_window);
        self.pending.retain(|_, pending| {
            now_unix_ms.saturating_sub(pending.observed_at_unix_ms) <= window_ms
        });
        let changed = self.prune(now_unix_ms);
        if exit_code != 0 {
            return self.observe_failure(session_id, command, stdout, stderr, now_unix_ms, changed);
        }
        self.observe_success(session_id, command, now_unix_ms, window_ms, changed)
    }

    fn observe_success(
        &mut self,
        session_id: Uuid,
        command: &str,
        now_unix_ms: i64,
        window_ms: i64,
        changed: bool,
    ) -> FailureMemoryObservation {
        let Some(pending) = self.pending.get(&session_id).cloned() else {
            return FailureMemoryObservation {
                recall: None,
                changed,
            };
        };
        let elapsed_ms = now_unix_ms.saturating_sub(pending.observed_at_unix_ms);
        if elapsed_ms > window_ms {
            self.pending.remove(&session_id);
            return FailureMemoryObservation {
                recall: None,
                changed,
            };
        }
        if is_observational(command) {
            return FailureMemoryObservation {
                recall: None,
                changed,
            };
        }

        self.pending.remove(&session_id);
        if command_shape(command, &self.forced_redactor) == pending.command_shape {
            // A retry that succeeds without an intervening command is likely
            // transient; there is no useful follow-up to remember.
            return FailureMemoryObservation {
                recall: None,
                changed,
            };
        }
        let (successful_follow_up, was_redacted) =
            safe_follow_up(command, &self.forced_redactor, &self.options.home_dir);
        if successful_follow_up.is_empty() {
            return FailureMemoryObservation {
                recall: None,
                changed,
            };
        }
        let reusable = !was_redacted;
        if let Some(entry) = self
            .snapshot
            .entries
            .iter_mut()
            .find(|entry| entry.fingerprint == pending.fingerprint)
        {
            entry.command_family = pending.command_family;
            entry.last_seen_unix_ms = now_unix_ms;
            entry.successful_follow_up = successful_follow_up;
            entry.reusable = reusable;
        } else {
            self.snapshot.entries.push(FailureFingerprint {
                fingerprint: pending.fingerprint,
                command_family: pending.command_family,
                occurrences: 1,
                last_seen_unix_ms: now_unix_ms,
                successful_follow_up,
                reusable,
            });
        }
        self.sort_and_bound();
        FailureMemoryObservation {
            recall: None,
            changed: true,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_failure(
        &mut self,
        session_id: Uuid,
        command: &str,
        stdout: Option<&str>,
        stderr: Option<&str>,
        now_unix_ms: i64,
        mut changed: bool,
    ) -> FailureMemoryObservation {
        let command_shape = command_shape(command, &self.forced_redactor);
        let command_family = command_family(command);
        let fingerprint = fingerprint_for(
            &command_shape,
            stdout,
            stderr,
            &self.forced_redactor,
            &self.options.home_dir,
        );
        let recall = self
            .snapshot
            .entries
            .iter_mut()
            .find(|entry| entry.fingerprint == fingerprint)
            .map(|entry| {
                entry.occurrences = entry.occurrences.saturating_add(1);
                entry.last_seen_unix_ms = now_unix_ms;
                changed = true;
                FailureMemoryRecall {
                    command_family: entry.command_family.clone(),
                    occurrences: entry.occurrences,
                    successful_follow_up: entry.successful_follow_up.clone(),
                    reusable: entry.reusable,
                }
            });
        self.pending.insert(
            session_id,
            PendingFailure {
                fingerprint,
                command_shape,
                command_family,
                observed_at_unix_ms: now_unix_ms,
            },
        );
        FailureMemoryObservation { recall, changed }
    }

    /// Atomically persist the bounded snapshot with owner-only permissions.
    ///
    /// # Errors
    ///
    /// Returns an error if the parent or snapshot cannot be securely written.
    pub fn persist(&self) -> Result<(), FailureMemoryError> {
        let path = &self.options.path;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| FailureMemoryError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|source| {
                FailureMemoryError::Io {
                    path: parent.to_path_buf(),
                    source,
                }
            })?;
        }
        let encoded = serde_json::to_vec_pretty(&self.snapshot)?;
        let temporary = parent.join(format!(".failure-memory.{}.tmp", Uuid::new_v4()));
        let write_result = (|| -> Result<(), FailureMemoryError> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temporary)
                .map_err(|source| FailureMemoryError::Io {
                    path: temporary.clone(),
                    source,
                })?;
            file.write_all(&encoded)
                .and_then(|()| file.write_all(b"\n"))
                .and_then(|()| file.sync_all())
                .map_err(|source| FailureMemoryError::Io {
                    path: temporary.clone(),
                    source,
                })?;
            fs::rename(&temporary, path).map_err(|source| FailureMemoryError::Io {
                path: path.clone(),
                source,
            })?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result
    }

    fn prune(&mut self, now_unix_ms: i64) -> bool {
        let cutoff = now_unix_ms.saturating_sub(duration_millis_i64(self.options.retention));
        let previous = self.snapshot.entries.len();
        self.snapshot
            .entries
            .retain(|entry| entry.last_seen_unix_ms >= cutoff);
        self.sort_and_bound();
        previous != self.snapshot.entries.len()
    }

    fn sort_and_bound(&mut self) {
        self.snapshot
            .entries
            .sort_by_key(|entry| std::cmp::Reverse(entry.last_seen_unix_ms));
        self.snapshot.entries.truncate(self.options.max_entries);
    }

    fn sanitize_loaded_entries(&mut self) -> bool {
        let mut changed = false;
        for entry in &mut self.snapshot.entries {
            let family = bounded_family(&entry.command_family);
            if family != entry.command_family {
                entry.command_family = family;
                changed = true;
            }
            let (follow_up, was_redacted) = safe_follow_up(
                &entry.successful_follow_up,
                &self.forced_redactor,
                &self.options.home_dir,
            );
            if follow_up != entry.successful_follow_up {
                entry.successful_follow_up = follow_up;
                changed = true;
            }
            if was_redacted && entry.reusable {
                entry.reusable = false;
                changed = true;
            }
        }
        changed
    }
}

fn validate_snapshot(
    path: &Path,
    snapshot: &FailureMemorySnapshot,
) -> Result<(), FailureMemoryError> {
    if snapshot.entries.len() > MAX_SCHEMA_ENTRIES {
        return Err(FailureMemoryError::InvalidEntry {
            path: path.to_path_buf(),
            index: MAX_SCHEMA_ENTRIES,
            reason: "entry count exceeds the hard limit",
        });
    }
    for (index, entry) in snapshot.entries.iter().enumerate() {
        let valid_hash = entry.fingerprint.len() == "sha256:".len() + 64
            && entry.fingerprint.starts_with("sha256:")
            && entry.fingerprint["sha256:".len()..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit());
        let reason = if !valid_hash {
            Some("fingerprint is not a SHA-256 identifier")
        } else if entry.command_family.is_empty()
            || entry.command_family.chars().count() > MAX_COMMAND_FAMILY_CHARS
        {
            Some("command family is empty or oversized")
        } else if strip_terminal_sequences(&entry.command_family, false) != entry.command_family {
            Some("command family contains terminal control characters")
        } else if entry.occurrences == 0 {
            Some("occurrence count must be positive")
        } else if entry.successful_follow_up.is_empty()
            || entry.successful_follow_up.chars().count() > MAX_STORED_COMMAND_CHARS
        {
            Some("successful follow-up is empty or oversized")
        } else if strip_terminal_sequences(&entry.successful_follow_up, false)
            != entry.successful_follow_up
        {
            Some("successful follow-up contains terminal control characters")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(FailureMemoryError::InvalidEntry {
                path: path.to_path_buf(),
                index,
                reason,
            });
        }
    }
    Ok(())
}

fn duration_millis_i64(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn fingerprint_for(
    command_shape: &str,
    stdout: Option<&str>,
    stderr: Option<&str>,
    redactor: &PrivacyRedactor,
    home_dir: &Path,
) -> String {
    let output = match (stdout.unwrap_or_default(), stderr.unwrap_or_default()) {
        ("", stderr) => stderr.to_owned(),
        (stdout, "") => stdout.to_owned(),
        (stdout, stderr) => format!("{stdout}\n{stderr}"),
    };
    let output_shape = diagnostic_shape(&output, redactor, home_dir);
    let mut hasher = Sha256::new();
    hasher.update(b"aicoach-failure-v1\0");
    hasher.update(command_shape.as_bytes());
    hasher.update(b"\0");
    hasher.update(output_shape.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn diagnostic_shape(value: &str, redactor: &PrivacyRedactor, home_dir: &Path) -> String {
    let scrubbed = redactor.redact(&strip_terminal_sequences(value, true));
    let scrubbed = replace_home(&scrubbed, home_dir);
    let bounded: String = scrubbed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_FINGERPRINT_OUTPUT_CHARS)
        .collect();
    normalize_volatile(&bounded.to_ascii_lowercase())
}

fn command_shape(command: &str, redactor: &PrivacyRedactor) -> String {
    let scrubbed = redactor.redact(&strip_terminal_sequences(command, false));
    let tokens = shell_words::split(&scrubbed)
        .unwrap_or_else(|_| scrubbed.split_whitespace().map(ToOwned::to_owned).collect());
    tokens
        .iter()
        .enumerate()
        .map(|(index, token)| {
            if index == 0 {
                return normalize_volatile(
                    &token
                        .rsplit('/')
                        .next()
                        .unwrap_or(token)
                        .to_ascii_lowercase(),
                );
            }
            if token.starts_with('-') {
                return token.split_once('=').map_or_else(
                    || normalize_volatile(token),
                    |(flag, _)| format!("{flag}=<value>"),
                );
            }
            if token.contains("://") {
                return "<url>".to_owned();
            }
            if token.starts_with('/') || token.starts_with("./") || token.starts_with("../") {
                return "<path>".to_owned();
            }
            if token.contains('=') {
                return token
                    .split_once('=')
                    .map_or_else(|| "<value>".to_owned(), |(key, _)| format!("{key}=<value>"));
            }
            normalize_volatile(&token.to_ascii_lowercase())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_volatile(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_digits = false;
    for character in value.chars() {
        if character.is_ascii_digit() {
            if !in_digits {
                output.push('#');
                in_digits = true;
            }
        } else {
            in_digits = false;
            output.push(if character.is_whitespace() {
                ' '
            } else {
                character
            });
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn command_family(command: &str) -> String {
    let tokens = shell_words::split(command).unwrap_or_default();
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        let base = token
            .rsplit('/')
            .next()
            .unwrap_or(token)
            .to_ascii_lowercase();
        if matches!(base.as_str(), "sudo" | "command" | "builtin" | "nohup") {
            index += 1;
            while index < tokens.len() && tokens[index].starts_with('-') {
                index += 1;
            }
            continue;
        }
        if base == "env" {
            index += 1;
            while index < tokens.len()
                && (tokens[index].contains('=') || tokens[index].starts_with('-'))
            {
                index += 1;
            }
            continue;
        }
        if token.contains('=') && !token.starts_with('=') {
            index += 1;
            continue;
        }
        let family = if base.is_empty() {
            "unknown".to_owned()
        } else {
            base
        };
        return bounded_family(&family);
    }
    "unknown".to_owned()
}

fn bounded_family(value: &str) -> String {
    strip_terminal_sequences(value, false)
        .chars()
        .take(MAX_COMMAND_FAMILY_CHARS)
        .collect()
}

fn safe_follow_up(command: &str, redactor: &PrivacyRedactor, home_dir: &Path) -> (String, bool) {
    let scrubbed = replace_home(&strip_terminal_sequences(command, false), home_dir);
    let redacted = redactor.redact(&scrubbed);
    let was_redacted = redacted != scrubbed;
    let bounded = strip_terminal_sequences(&redacted, false)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_STORED_COMMAND_CHARS)
        .collect();
    (bounded, was_redacted)
}

fn replace_home(value: &str, home_dir: &Path) -> String {
    let home = home_dir.to_string_lossy();
    if home.is_empty() || home == "/" {
        value.to_owned()
    } else {
        value.replace(home.as_ref(), "~")
    }
}

fn is_observational(command: &str) -> bool {
    if command.contains('>') || command.contains('|') {
        return false;
    }
    let tokens = shell_words::split(command).unwrap_or_default();
    let family = command_family(command);
    let subcommand = tokens
        .iter()
        .skip_while(|token| token.rsplit('/').next().unwrap_or(token) != family)
        .nth(1)
        .map(String::as_str);
    match family.as_str() {
        "cd" | "pwd" | "ls" | "cat" | "less" | "head" | "tail" | "which" | "type" | "history"
        | "clear" => true,
        "git" => matches!(subcommand, Some("status" | "diff" | "log" | "show")),
        "docker" => matches!(
            subcommand,
            Some("ps" | "images" | "logs" | "inspect" | "info")
        ),
        _ => false,
    }
}

#[derive(Debug, Error)]
pub enum FailureMemoryError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid failure-memory JSON in {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("unsupported failure-memory version {version} in {path}")]
    UnsupportedVersion { path: PathBuf, version: u32 },
    #[error("failure-memory file at {path} is too large ({bytes} bytes)")]
    TooLarge { path: PathBuf, bytes: u64 },
    #[error("invalid failure-memory entry {index} in {path}: {reason}")]
    InvalidEntry {
        path: PathBuf,
        index: usize,
        reason: &'static str,
    },
    #[error("could not serialize failure memory: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn memory(directory: &Path) -> FailureMemory {
        FailureMemory::load(FailureMemoryOptions {
            path: directory.join("failure-memory.json"),
            home_dir: PathBuf::from("/Users/alice"),
            max_entries: 2,
            retention: Duration::from_secs(30 * 24 * 60 * 60),
            resolution_window: Duration::from_secs(10 * 60),
            redactor: PrivacyRedactor::default(),
        })
        .unwrap()
    }

    #[test]
    fn remembers_the_next_non_observational_success() {
        let directory = tempfile::tempdir().unwrap();
        let mut memory = memory(directory.path());
        let session = Uuid::new_v4();
        assert!(
            memory
                .observe(
                    session,
                    "cargo test broken_42",
                    101,
                    None,
                    Some("error at /Users/alice/src/main.rs:42"),
                    1_000,
                )
                .recall
                .is_none()
        );
        memory.observe(session, "git status", 0, None, None, 2_000);
        let stored = memory.observe(session, "cargo fmt", 0, None, None, 3_000);
        assert!(stored.changed);
        memory.persist().unwrap();

        let snapshot =
            FailureMemorySnapshot::load(directory.path().join("failure-memory.json")).unwrap();
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].successful_follow_up, "cargo fmt");

        let recall = memory
            .observe(
                session,
                "cargo test broken_99",
                101,
                None,
                Some("error at /Users/alice/src/main.rs:99"),
                4_000,
            )
            .recall
            .unwrap();
        assert_eq!(recall.successful_follow_up, "cargo fmt");
        assert_eq!(recall.occurrences, 2);
    }

    #[test]
    fn disk_never_contains_the_failed_command_or_raw_diagnostic() {
        let directory = tempfile::tempdir().unwrap();
        let mut memory = memory(directory.path());
        let session = Uuid::new_v4();
        memory.observe(
            session,
            "deploy customer-secret-project",
            1,
            None,
            Some("private diagnostic customer-123"),
            1_000,
        );
        memory.observe(
            session,
            "TOKEN=github_pat_abcdefghijklmnopqrstuvwxyz1234567890 deploy --retry",
            0,
            None,
            None,
            2_000,
        );
        memory.persist().unwrap();
        let encoded = fs::read_to_string(directory.path().join("failure-memory.json")).unwrap();
        assert!(!encoded.contains("customer-secret-project"));
        assert!(!encoded.contains("private diagnostic"));
        assert!(!encoded.contains("github_pat_"));
        assert!(encoded.contains("[REDACTED]"));
        assert!(!memory.entries()[0].reusable);
    }

    #[test]
    fn store_is_bounded_private_and_expires_old_entries() {
        let directory = tempfile::tempdir().unwrap();
        let mut memory = memory(directory.path());
        let session = Uuid::new_v4();
        for (index, name) in [(0_i64, "alpha"), (1, "bravo"), (2, "charlie")] {
            memory.observe(
                session,
                &format!("tool-{name} fail"),
                1,
                None,
                Some(&format!("error-{index}")),
                index * 1_000,
            );
            memory.observe(
                session,
                &format!("tool-{name} repair"),
                0,
                None,
                None,
                index * 1_000 + 1,
            );
        }
        assert_eq!(memory.entries().len(), 2);
        memory.persist().unwrap();
        let path = directory.path().join("failure-memory.json");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let later = 31_i64 * 24 * 60 * 60 * 1_000;
        assert!(
            memory
                .observe(session, "true", 0, None, None, later)
                .changed
        );
        assert!(memory.entries().is_empty());
    }

    #[test]
    fn malformed_store_is_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("failure-memory.json");
        fs::write(&path, "not json").unwrap();
        let result = FailureMemory::load(FailureMemoryOptions {
            path: path.clone(),
            home_dir: PathBuf::from("/Users/alice"),
            max_entries: 2,
            retention: Duration::from_secs(60),
            resolution_window: Duration::from_secs(60),
            redactor: PrivacyRedactor::default(),
        });
        assert!(matches!(result, Err(FailureMemoryError::Parse { .. })));
        assert_eq!(fs::read_to_string(path).unwrap(), "not json");
    }

    #[test]
    fn oversized_store_is_rejected_before_decoding() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("failure-memory.json");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_MEMORY_FILE_BYTES + 1).unwrap();
        assert!(matches!(
            FailureMemorySnapshot::load(&path),
            Err(FailureMemoryError::TooLarge { .. })
        ));
        assert_eq!(fs::metadata(path).unwrap().len(), MAX_MEMORY_FILE_BYTES + 1);
    }

    #[test]
    fn semantically_invalid_store_is_not_rewritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("failure-memory.json");
        let invalid = r#"{
          "version": 1,
          "entries": [{
            "fingerprint": "not-a-hash",
            "commandFamily": "cargo",
            "occurrences": 1,
            "lastSeenUnixMs": 1,
            "successfulFollowUp": "cargo fmt",
            "reusable": true
          }]
        }"#;
        fs::write(&path, invalid).unwrap();
        assert!(matches!(
            FailureMemorySnapshot::load(&path),
            Err(FailureMemoryError::InvalidEntry { .. })
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), invalid);
    }

    #[test]
    fn recall_survives_restart_and_honors_custom_redaction_patterns() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("failure-memory.json");
        let mut privacy = crate::PrivacyConfig::default();
        privacy.extra_patterns.push(r"customer-[a-z]+".to_owned());
        privacy.replacement = "\u{1b}[31m[PRIVATE]\u{1b}[0m".to_owned();
        let original_options = FailureMemoryOptions {
            path: path.clone(),
            home_dir: PathBuf::from("/Users/alice"),
            max_entries: 8,
            retention: Duration::from_secs(30 * 24 * 60 * 60),
            resolution_window: Duration::from_secs(10 * 60),
            redactor: PrivacyRedactor::default(),
        };
        let session = Uuid::new_v4();
        let now = Utc::now().timestamp_millis();
        let mut first = FailureMemory::load(original_options.clone()).unwrap();
        first.observe(
            session,
            "zztool fail",
            1,
            None,
            Some("same diagnostic"),
            now,
        );
        first.observe(
            session,
            "zztool repair customer-alpha",
            0,
            None,
            None,
            now + 1,
        );
        first.persist().unwrap();
        drop(first);

        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("customer-alpha")
        );
        let mut updated_options = original_options;
        updated_options.redactor = PrivacyRedactor::new(&privacy).unwrap();
        let mut restarted = FailureMemory::load(updated_options).unwrap();
        assert_eq!(
            restarted.entries()[0].successful_follow_up,
            "zztool repair [PRIVATE]"
        );
        let recall = restarted
            .observe(
                session,
                "zztool fail",
                1,
                None,
                Some("same diagnostic"),
                now + 2,
            )
            .recall
            .unwrap();
        assert_eq!(recall.successful_follow_up, "zztool repair [PRIVATE]");
        assert!(!recall.reusable);
    }
}

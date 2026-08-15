use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use chrono::Utc;
use uuid::Uuid;

use crate::config::ContextConfig;
use crate::models::{CommandRecord, GitContext, TerminalContext};

const TRUNCATION_MARKER: &str = "\n… [output truncated] …\n";

/// Bounded, in-memory terminal history for a single shell session.
///
/// The manager never writes history to disk. Persistence is an opt-in concern
/// for the daemon and is intentionally separate from this latency-sensitive
/// component.
#[derive(Debug, Clone)]
pub struct ContextManager {
    config: ContextConfig,
    commands: VecDeque<CommandRecord>,
    total_chars: usize,
    evicted_commands: usize,
    eviction_summaries: VecDeque<String>,
}

impl ContextManager {
    pub fn new(config: ContextConfig) -> Self {
        Self {
            config,
            commands: VecDeque::with_capacity(config.max_commands),
            total_chars: 0,
            evicted_commands: 0,
            eviction_summaries: VecDeque::new(),
        }
    }

    pub fn config(&self) -> ContextConfig {
        self.config
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub fn total_chars(&self) -> usize {
        self.total_chars
    }

    pub fn evicted_commands(&self) -> usize {
        self.evicted_commands
    }

    pub fn latest(&self) -> Option<&CommandRecord> {
        self.commands.back()
    }

    pub fn records(&self) -> impl DoubleEndedIterator<Item = &CommandRecord> {
        self.commands.iter()
    }

    /// Add a completed or in-flight command. Oversized output is summarized by
    /// retaining its beginning and end, after which the oldest records are
    /// evicted until both configured bounds hold.
    pub fn push(&mut self, mut record: CommandRecord) {
        limit_record_output(&mut record, self.config.max_output_per_command);
        fit_record_to_total_budget(&mut record, self.config.max_total_chars);

        // An extraordinary cwd can exceed the complete budget by itself. It is
        // safer to omit that unusable record than violate a documented bound.
        if record.char_len() > self.config.max_total_chars {
            self.note_eviction(&record);
            return;
        }

        self.total_chars += record.char_len();
        self.commands.push_back(record);
        self.enforce_limits();
    }

    pub fn add_record(&mut self, record: CommandRecord) {
        self.push(record);
    }

    pub fn extend<I>(&mut self, records: I)
    where
        I: IntoIterator<Item = CommandRecord>,
    {
        for record in records {
            self.push(record);
        }
    }

    pub fn clear(&mut self) {
        self.commands.clear();
        self.eviction_summaries.clear();
        self.total_chars = 0;
        self.evicted_commands = 0;
    }

    pub fn snapshot(
        &self,
        session_id: Uuid,
        cwd: impl Into<PathBuf>,
        shell: impl Into<String>,
    ) -> TerminalContext {
        self.snapshot_with_git(session_id, cwd, shell, None)
    }

    pub fn snapshot_with_git(
        &self,
        session_id: Uuid,
        cwd: impl Into<PathBuf>,
        shell: impl Into<String>,
        git: Option<GitContext>,
    ) -> TerminalContext {
        let commands: Vec<_> = self.commands.iter().cloned().collect();
        let updated_at = commands
            .last()
            .map_or_else(Utc::now, |record| record.timestamp);
        let started_at = commands
            .first()
            .map_or(updated_at, |record| record.timestamp);
        TerminalContext {
            session_id,
            cwd: cwd.into(),
            shell: shell.into(),
            started_at,
            updated_at,
            commands,
            git,
            environment: BTreeMap::default(),
        }
    }

    /// Compact prompt text for recent commands. The newest context is retained
    /// when `max_chars` requires truncation.
    pub fn summarize(&self, max_chars: usize) -> String {
        if max_chars == 0 {
            return String::new();
        }
        let mut sections = VecDeque::new();
        let mut used = 0;
        for record in self.commands.iter().rev() {
            let section = record.summary();
            let separator = usize::from(!sections.is_empty());
            let section_len = section.chars().count();
            if section_len + separator <= max_chars.saturating_sub(used) {
                sections.push_front(section);
                used += section_len + separator;
                continue;
            }
            if sections.is_empty() {
                sections.push_front(truncate_middle(&section, max_chars));
            }
            break;
        }
        sections.into_iter().collect::<Vec<_>>().join("\n")
    }

    pub fn eviction_summaries(&self) -> impl Iterator<Item = &str> {
        self.eviction_summaries.iter().map(String::as_str)
    }

    fn enforce_limits(&mut self) {
        while self.commands.len() > self.config.max_commands
            || self.total_chars > self.config.max_total_chars
        {
            let Some(record) = self.commands.pop_front() else {
                break;
            };
            self.total_chars = self.total_chars.saturating_sub(record.char_len());
            self.note_eviction(&record);
        }
    }

    fn note_eviction(&mut self, record: &CommandRecord) {
        self.evicted_commands += 1;
        let status = record
            .exit_code
            .map_or_else(|| "running".to_owned(), |code| format!("exit {code}"));
        self.eviction_summaries.push_back(format!(
            "$ {} ({status})",
            truncate_middle(&record.command, 160)
        ));
        while self.eviction_summaries.len() > self.config.max_commands {
            self.eviction_summaries.pop_front();
        }
    }
}

impl Default for ContextManager {
    fn default() -> Self {
        Self::new(ContextConfig::default())
    }
}

fn limit_record_output(record: &mut CommandRecord, max_chars: usize) {
    let stdout_chars = record.stdout.chars().count();
    let stderr_chars = record.stderr.chars().count();
    if stdout_chars + stderr_chars <= max_chars {
        return;
    }

    // Preserve stderr preferentially because it generally contains the failure
    // cause, while still retaining at least a quarter of the budget for stdout.
    let stderr_budget = stderr_chars.min(max_chars.saturating_mul(3) / 4);
    let stdout_budget = stdout_chars.min(max_chars.saturating_sub(stderr_budget));
    let unused = max_chars.saturating_sub(stderr_budget + stdout_budget);
    let final_stderr_budget = (stderr_budget + unused).min(stderr_chars);
    let final_stdout_budget = max_chars.saturating_sub(final_stderr_budget);
    record.stdout = truncate_output(&record.stdout, final_stdout_budget);
    record.stderr = truncate_output(&record.stderr, final_stderr_budget);

    // Markers themselves use budget. A second deterministic pass guarantees
    // the configured output cap even for very small custom limits.
    let actual = record.stdout.chars().count() + record.stderr.chars().count();
    if actual > max_chars {
        let stderr_budget = record.stderr.chars().count().min(max_chars);
        record.stderr = truncate_middle(&record.stderr, stderr_budget);
        record.stdout = truncate_middle(
            &record.stdout,
            max_chars.saturating_sub(record.stderr.chars().count()),
        );
    }
}

fn fit_record_to_total_budget(record: &mut CommandRecord, max_chars: usize) {
    if record.char_len() <= max_chars {
        return;
    }
    let non_output =
        record.char_len() - record.stdout.chars().count() - record.stderr.chars().count();
    let output_budget = max_chars.saturating_sub(non_output);
    limit_record_output(record, output_budget);
    if record.char_len() <= max_chars {
        return;
    }

    // Output may already be empty. Keep both ends of long generated commands,
    // where flags and redirections are often more useful than the middle.
    let without_command = record.char_len() - record.command.chars().count();
    record.command = truncate_middle(&record.command, max_chars.saturating_sub(without_command));
}

fn truncate_output(value: &str, max_chars: usize) -> String {
    let length = value.chars().count();
    if length <= max_chars {
        return value.to_owned();
    }
    let marker_length = TRUNCATION_MARKER.chars().count();
    if max_chars <= marker_length {
        return truncate_middle(value, max_chars);
    }
    let content_budget = max_chars - marker_length;
    let head = content_budget / 2;
    let tail = content_budget - head;
    let beginning: String = value.chars().take(head).collect();
    let ending: String = value.chars().skip(length - tail).collect();
    format!("{beginning}{TRUNCATION_MARKER}{ending}")
}

fn truncate_middle(value: &str, max_chars: usize) -> String {
    let length = value.chars().count();
    if length <= max_chars {
        return value.to_owned();
    }
    match max_chars {
        0 => String::new(),
        1 => "…".to_owned(),
        _ => {
            let content = max_chars - 1;
            let head = content / 2;
            let tail = content - head;
            let beginning: String = value.chars().take(head).collect();
            let ending: String = value.chars().skip(length - tail).collect();
            format!("{beginning}…{ending}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(index: usize, output: &str) -> CommandRecord {
        CommandRecord::completed(
            format!("command-{index}"),
            "/tmp",
            i32::try_from(index).unwrap(),
            output,
            "",
        )
    }

    #[test]
    fn evicts_oldest_by_command_count() {
        let mut manager = ContextManager::new(ContextConfig {
            max_commands: 2,
            max_output_per_command: 1_000,
            max_total_chars: 2_000,
        });
        manager.extend([record(1, "one"), record(2, "two"), record(3, "three")]);
        assert_eq!(manager.len(), 2);
        assert_eq!(manager.evicted_commands(), 1);
        assert_eq!(manager.records().next().unwrap().command, "command-2");
        assert!(
            manager
                .eviction_summaries()
                .next()
                .unwrap()
                .contains("command-1")
        );
    }

    #[test]
    fn truncates_output_by_character_count_and_keeps_both_ends() {
        let mut manager = ContextManager::new(ContextConfig {
            max_commands: 5,
            max_output_per_command: 80,
            max_total_chars: 1_000,
        });
        let output = format!("START{}END", "中".repeat(200));
        manager.push(record(1, &output));
        let stored = manager.latest().unwrap();
        assert!(stored.stdout.starts_with("START"));
        assert!(stored.stdout.ends_with("END"));
        assert!(stored.stdout.contains("output truncated"));
        assert!(stored.stdout.chars().count() <= 80);
    }

    #[test]
    fn total_budget_evicts_oldest_and_never_exceeds_limit() {
        let mut manager = ContextManager::new(ContextConfig {
            max_commands: 20,
            max_output_per_command: 100,
            max_total_chars: 80,
        });
        for index in 0..10 {
            manager.push(record(index, "some output"));
            assert!(manager.total_chars() <= 80);
        }
        assert!(manager.len() < 10);
        assert_eq!(
            manager.total_chars(),
            manager
                .records()
                .map(CommandRecord::char_len)
                .sum::<usize>()
        );
    }

    #[test]
    fn prompt_summary_keeps_the_newest_records_within_budget() {
        let mut manager = ContextManager::default();
        manager.extend([record(1, "old output"), record(2, "new output")]);
        let summary = manager.summarize(35);
        assert!(summary.chars().count() <= 35);
        assert!(summary.contains("command-2"));
    }

    #[test]
    fn snapshot_preserves_session_isolation_and_git() {
        let mut manager = ContextManager::default();
        manager.push(record(1, "ok"));
        let session_id = Uuid::new_v4();
        let git = GitContext {
            repo_root: PathBuf::from("/tmp/repo"),
            branch: Some("main".to_owned()),
            ..GitContext::default()
        };
        let snapshot = manager.snapshot_with_git(session_id, "/tmp/repo", "zsh", Some(git.clone()));
        assert_eq!(snapshot.session_id, session_id);
        assert_eq!(snapshot.commands.len(), 1);
        assert_eq!(snapshot.git, Some(git));
    }

    #[test]
    fn zero_length_summary_is_empty() {
        let mut manager = ContextManager::default();
        manager.push(record(1, "ok"));
        assert!(manager.summarize(0).is_empty());
    }
}

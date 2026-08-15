use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandRecord {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub command: String,
    pub cwd: PathBuf,
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub interactive: bool,
}

impl CommandRecord {
    pub fn new(command: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            id: Uuid::new_v4(),
            timestamp: Utc::now(),
            command: command.into(),
            cwd: cwd.into(),
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            duration_ms: None,
            interactive: false,
        }
    }

    pub fn completed(
        command: impl Into<String>,
        cwd: impl Into<PathBuf>,
        exit_code: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Self {
            exit_code: Some(exit_code),
            stdout: stdout.into(),
            stderr: stderr.into(),
            ..Self::new(command, cwd)
        }
    }

    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    pub fn failed(&self) -> bool {
        self.exit_code.is_some_and(|code| code != 0)
    }

    pub fn char_len(&self) -> usize {
        self.command.chars().count()
            + self.cwd.to_string_lossy().chars().count()
            + self.stdout.chars().count()
            + self.stderr.chars().count()
    }

    /// A compact, deterministic representation suitable for an AI prompt.
    pub fn summary(&self) -> String {
        let status = self
            .exit_code
            .map_or_else(|| "running".to_owned(), |code| format!("exit {code}"));
        let mut summary = format!("$ {} ({status})", self.command);
        if !self.stderr.is_empty() {
            summary.push_str("\nstderr: ");
            summary.push_str(self.stderr.trim());
        } else if !self.stdout.is_empty() {
            summary.push_str("\nstdout: ");
            summary.push_str(self.stdout.trim());
        }
        summary
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitContext {
    pub repo_root: PathBuf,
    pub branch: Option<String>,
    #[serde(default)]
    pub detached: bool,
    #[serde(default)]
    pub modified_files: usize,
    #[serde(default)]
    pub staged_files: usize,
    #[serde(default)]
    pub untracked_files: usize,
    #[serde(default)]
    pub conflicts: usize,
    #[serde(default)]
    pub ahead: usize,
    #[serde(default)]
    pub behind: usize,
    pub remote: Option<String>,
}

impl GitContext {
    pub fn is_clean(&self) -> bool {
        self.modified_files == 0
            && self.staged_files == 0
            && self.untracked_files == 0
            && self.conflicts == 0
    }

    pub fn status_summary(&self) -> String {
        let branch = self.branch.as_deref().unwrap_or("detached HEAD");
        if self.is_clean() {
            return format!("{branch}: clean");
        }
        let mut parts = Vec::new();
        if self.staged_files > 0 {
            parts.push(format!("{} staged", self.staged_files));
        }
        if self.modified_files > 0 {
            parts.push(format!("{} modified", self.modified_files));
        }
        if self.untracked_files > 0 {
            parts.push(format!("{} untracked", self.untracked_files));
        }
        if self.conflicts > 0 {
            parts.push(format!("{} conflicted", self.conflicts));
        }
        format!("{branch}: {}", parts.join(", "))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalContext {
    pub session_id: Uuid,
    pub cwd: PathBuf,
    pub shell: String,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub commands: Vec<CommandRecord>,
    pub git: Option<GitContext>,
    /// Only explicitly selected, non-secret environment metadata belongs here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

impl TerminalContext {
    pub fn new(session_id: Uuid, cwd: impl Into<PathBuf>, shell: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            session_id,
            cwd: cwd.into(),
            shell: shell.into(),
            started_at: now,
            updated_at: now,
            commands: Vec::new(),
            git: None,
            environment: BTreeMap::new(),
        }
    }

    pub fn latest_command(&self) -> Option<&CommandRecord> {
        self.commands.last()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CompletionOperation {
    Replace,
    Insert,
    Suggest,
    #[default]
    None,
}

impl fmt::Display for CompletionOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Replace => "replace",
            Self::Insert => "insert",
            Self::Suggest => "suggest",
            Self::None => "none",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompletionInput {
    pub buffer: String,
    /// Character offset, matching ZLE's `CURSOR` semantics (not a byte offset).
    pub cursor: usize,
    pub cwd: PathBuf,
    pub shell: String,
    #[serde(default)]
    pub context: Vec<String>,
}

impl CompletionInput {
    pub fn new(
        buffer: impl Into<String>,
        cursor: usize,
        cwd: impl Into<PathBuf>,
        shell: impl Into<String>,
    ) -> Self {
        Self {
            buffer: buffer.into(),
            cursor,
            cwd: cwd.into(),
            shell: shell.into(),
            context: Vec::new(),
        }
    }

    pub fn clamped_cursor(&self) -> usize {
        self.cursor.min(self.buffer.chars().count())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CompletionResult {
    #[serde(rename = "type", alias = "operation")]
    pub operation: CompletionOperation,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub cursor: usize,
    #[serde(default)]
    pub description: String,
}

impl CompletionResult {
    pub fn replace(command: impl Into<String>, description: impl Into<String>) -> Self {
        let command = command.into();
        let cursor = command.chars().count();
        Self {
            operation: CompletionOperation::Replace,
            command,
            cursor,
            description: description.into(),
        }
    }

    /// Apply the structured operation without ever executing the command.
    /// Returns the resulting buffer and a character-index cursor.
    pub fn apply_to(&self, buffer: &str, input_cursor: usize) -> (String, usize) {
        match self.operation {
            CompletionOperation::Replace => {
                let max_cursor = self.command.chars().count();
                (self.command.clone(), self.cursor.min(max_cursor))
            }
            CompletionOperation::Insert => {
                let insertion = input_cursor.min(buffer.chars().count());
                let byte_offset = char_to_byte_index(buffer, insertion);
                let mut result = String::with_capacity(buffer.len() + self.command.len());
                result.push_str(&buffer[..byte_offset]);
                result.push_str(&self.command);
                result.push_str(&buffer[byte_offset..]);
                let default_cursor = insertion + self.command.chars().count();
                let requested = if self.cursor == 0 {
                    default_cursor
                } else {
                    self.cursor
                };
                let max_cursor = result.chars().count();
                (result, requested.min(max_cursor))
            }
            CompletionOperation::Suggest | CompletionOperation::None => {
                (buffer.to_owned(), input_cursor.min(buffer.chars().count()))
            }
        }
    }
}

fn char_to_byte_index(value: &str, character_index: usize) -> usize {
    value
        .char_indices()
        .nth(character_index)
        .map_or(value.len(), |(index, _)| index)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    #[default]
    Info,
    Warning,
    Error,
    Critical,
}

impl fmt::Display for Severity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisCategory {
    CommandNotFound,
    PermissionDenied,
    FileNotFound,
    Git,
    Docker,
    Network,
    Compiler,
    Ssh,
    PackageManager,
    Spelling,
    DangerousCommand,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisInput {
    pub os: String,
    pub shell: String,
    pub cwd: PathBuf,
    pub command: String,
    pub exit_code: i32,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub context: Vec<CommandRecord>,
    /// Fixed, non-secret shell metadata allowlisted by the integration layer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// Allowlisted metadata changed since the previous completed command.
    /// `None` means that a previously present variable was unset.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment_changes: BTreeMap<String, Option<String>>,
    pub git: Option<GitContext>,
}

impl AnalysisInput {
    pub fn new(command: impl Into<String>, exit_code: i32, cwd: impl Into<PathBuf>) -> Self {
        Self {
            os: "macOS".to_owned(),
            shell: "zsh".to_owned(),
            cwd: cwd.into(),
            command: command.into(),
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
            context: Vec::new(),
            environment: BTreeMap::new(),
            environment_changes: BTreeMap::new(),
            git: None,
        }
    }

    pub fn from_record(record: &CommandRecord, context: Vec<CommandRecord>) -> Self {
        Self {
            command: record.command.clone(),
            exit_code: record.exit_code.unwrap_or(-1),
            cwd: record.cwd.clone(),
            stdout: record.stdout.clone(),
            stderr: record.stderr.clone(),
            context,
            ..Self::new("", -1, PathBuf::new())
        }
    }

    pub fn combined_output(&self) -> String {
        match (self.stdout.trim(), self.stderr.trim()) {
            ("", stderr) => stderr.to_owned(),
            (stdout, "") => stdout.to_owned(),
            (stdout, stderr) => format!("{stdout}\n{stderr}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisResult {
    pub need_response: bool,
    pub severity: Severity,
    #[serde(default)]
    pub category: AnalysisCategory,
    pub title: String,
    pub message: String,
    pub suggested_command: Option<String>,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

const fn default_confidence() -> f32 {
    1.0
}

impl AnalysisResult {
    pub fn no_response() -> Self {
        Self {
            need_response: false,
            severity: Severity::Info,
            category: AnalysisCategory::Unknown,
            title: String::new(),
            message: String::new(),
            suggested_command: None,
            confidence: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(ChatRole::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(ChatRole::Assistant, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_schema_uses_type_and_character_cursor() {
        let result = CompletionResult::replace("echo 你好", "test");
        assert_eq!(result.cursor, 7);
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["type"], "replace");
        assert!(json.get("operation").is_none());
        let decoded: CompletionResult = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, result);
    }

    #[test]
    fn insert_completion_handles_unicode_without_panicking() {
        let result = CompletionResult {
            operation: CompletionOperation::Insert,
            command: "好".to_owned(),
            cursor: 0,
            description: String::new(),
        };
        let (buffer, cursor) = result.apply_to("你 world", 1);
        assert_eq!(buffer, "你好 world");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn analysis_schema_matches_camel_case_contract() {
        let input = AnalysisInput::new("false", 1, "/tmp");
        let json = serde_json::to_value(input).unwrap();
        assert_eq!(json["exitCode"], 1);
        assert!(json.get("exit_code").is_none());
    }

    #[test]
    fn git_summary_is_compact() {
        let context = GitContext {
            branch: Some("feature/test".to_owned()),
            modified_files: 2,
            untracked_files: 1,
            ..GitContext::default()
        };
        assert_eq!(
            context.status_summary(),
            "feature/test: 2 modified, 1 untracked"
        );
    }
}

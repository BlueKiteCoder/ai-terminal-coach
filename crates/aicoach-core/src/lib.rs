//! Shared domain logic for AI Terminal Coach.
//!
//! This crate deliberately contains no async runtime or network client.  Its
//! operations are suitable for latency-sensitive shell hooks and can be reused
//! by the daemon, CLI, IPC protocol, and TUI.

pub mod analyzer;
pub mod config;
pub mod context;
pub mod git;
pub mod models;
pub mod privacy;
pub mod safety;
pub mod terminal;

pub use analyzer::{AnalysisCategory, LocalAnalysis, LocalAnalyzer};
pub use config::{
    AiConfig, AiModels, AiModelsConfig, AiTimeouts, AiTimeoutsConfig, CoachConfig, Config,
    ConfigError, ContextConfig, HistoryConfig, KeybindingsConfig, PrivacyConfig, ProductPaths,
    SafetyConfig, SafetyMode, WindowConfig,
};
pub use context::ContextManager;
pub use git::{GitContextError, collect_git_context, try_collect_git_context};
pub use models::{
    AnalysisInput, AnalysisResult, ChatMessage, ChatRole, CommandRecord, CompletionInput,
    CompletionOperation, CompletionResult, GitContext, Severity, TerminalContext,
};
pub use privacy::{PrivacyError, PrivacyRedactor};
pub use safety::{RiskLevel, SafetyAssessment, SafetyEngine, SafetyFinding};
pub use terminal::strip_terminal_sequences;

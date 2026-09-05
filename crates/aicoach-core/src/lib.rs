//! Shared domain logic for AI Terminal Coach.
//!
//! This crate deliberately contains no async runtime or network client.  Its
//! operations are suitable for latency-sensitive shell hooks and can be reused
//! by the daemon, CLI, IPC protocol, and TUI.

pub const MAX_DAEMON_LOG_FILES: usize = 48;
pub const MAX_PERSISTED_HISTORY_SESSIONS: usize = 20;

pub mod analyzer;
pub mod command_patch;
pub mod config;
pub mod context;
pub mod environment_drift;
pub mod failure_memory;
pub mod git;
pub mod models;
pub mod privacy;
pub mod risk_lens;
pub mod safety;
pub mod source_cards;
pub mod terminal;

pub use analyzer::{AnalysisCategory, LocalAnalysis, LocalAnalyzer};
pub use command_patch::{CommandPatch, CommandPatchHunk};
pub use config::{
    AiConfig, AiModels, AiModelsConfig, AiTimeouts, AiTimeoutsConfig, CoachConfig, Config,
    ConfigError, ContextConfig, HistoryConfig, KeybindingsConfig, MemoryConfig, PrivacyConfig,
    ProductPaths, SafetyConfig, SafetyMode, WindowConfig,
};
pub use context::ContextManager;
pub use environment_drift::{
    EnvironmentDrift, EnvironmentDriftKind, EnvironmentDriftReport, EnvironmentSnapshot,
};
pub use failure_memory::{
    FailureFingerprint, FailureMemory, FailureMemoryError, FailureMemoryObservation,
    FailureMemoryOptions, FailureMemoryRecall, FailureMemorySnapshot,
};
pub use git::{GitContextError, collect_git_context, try_collect_git_context};
pub use models::{
    AnalysisInput, AnalysisResult, ChatMessage, ChatRole, CommandRecord, CompletionInput,
    CompletionOperation, CompletionResult, GitContext, Severity, TerminalContext,
};
pub use privacy::{PrivacyError, PrivacyRedactor};
pub use risk_lens::{
    AnalysisCoverage, EffectAction, PrivilegeRequirement, RecoveryProspect, RiskEffect,
    RiskLensReport,
};
pub use safety::{RiskLevel, SafetyAssessment, SafetyEngine, SafetyFinding};
pub use source_cards::{
    SourceCard, SourceInvocation, SourceOrigin, SourceQuery, source_card_from_output,
    source_queries,
};
pub use terminal::strip_terminal_sequences;

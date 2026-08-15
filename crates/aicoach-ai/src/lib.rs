//! AI provider abstraction and an OpenAI-compatible implementation.
//!
//! The client deliberately keeps credentials out of request/response errors and
//! tracing fields. Production construction reads a key from the environment
//! variable named by [`OpenAiConfig::api_key_env`].

mod error;
mod openai;
mod provider;
mod types;

pub use error::{AiError, AiOperation, AiResult};
pub use openai::OpenAiCompatibleProvider;
pub use provider::{AiProvider, NoopAiProvider};
pub use types::{
    AiModels, AiTimeouts, ChatRequest, ChatResponse, ChatStream, CommandCompletionRequest,
    OpenAiConfig, RetryPolicy, TokenUsage,
};

pub use aicoach_core::{
    AnalysisInput, AnalysisResult, ChatMessage, ChatRole, CompletionInput, CompletionResult,
};

#[cfg(test)]
mod tests;

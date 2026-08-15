use std::{pin::Pin, time::Duration};

use futures_util::Stream;
use serde::{Deserialize, Serialize};

use crate::{AiResult, ChatMessage};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AiModels {
    pub completion: String,
    pub analysis: String,
    pub chat: String,
}

impl Default for AiModels {
    fn default() -> Self {
        Self {
            completion: "gpt-4.1-mini".to_owned(),
            analysis: "gpt-4.1-mini".to_owned(),
            chat: "gpt-4.1".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AiTimeouts {
    pub completion: Duration,
    pub analysis: Duration,
    pub chat: Duration,
}

impl Default for AiTimeouts {
    fn default() -> Self {
        Self {
            completion: Duration::from_secs(3),
            analysis: Duration::from_secs(15),
            chat: Duration::from_secs(60),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    /// Number of retries after the first attempt.
    pub max_retries: usize,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 2,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(1),
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpenAiConfig {
    pub base_url: String,
    pub api_key_env: String,
    pub models: AiModels,
    pub temperature: f32,
    pub timeouts: AiTimeouts,
    pub max_concurrency: usize,
    pub retry: RetryPolicy,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key_env: "AI_COACH_API_KEY".to_owned(),
            models: AiModels::default(),
            temperature: 0.2,
            timeouts: AiTimeouts::default(),
            max_concurrency: 4,
            retry: RetryPolicy::default(),
        }
    }
}

impl From<&aicoach_core::AiConfig> for OpenAiConfig {
    fn from(config: &aicoach_core::AiConfig) -> Self {
        Self {
            base_url: config.base_url.clone(),
            api_key_env: config.api_key_env.clone(),
            models: AiModels {
                completion: config.models.completion.clone(),
                analysis: config.models.error_analysis.clone(),
                chat: config.models.chat.clone(),
            },
            temperature: config.temperature,
            timeouts: AiTimeouts {
                completion: Duration::from_millis(config.timeouts_ms.completion),
                analysis: Duration::from_millis(config.timeouts_ms.error_analysis),
                chat: Duration::from_millis(config.timeouts_ms.chat),
            },
            max_concurrency: config.max_concurrent_requests,
            retry: RetryPolicy::default(),
        }
    }
}

impl From<aicoach_core::AiConfig> for OpenAiConfig {
    fn from(config: aicoach_core::AiConfig) -> Self {
        Self::from(&config)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
}

impl ChatRequest {
    pub fn new(messages: impl IntoIterator<Item = ChatMessage>) -> Self {
        Self {
            messages: messages.into_iter().collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChatResponse {
    pub content: String,
    pub model: Option<String>,
    pub finish_reason: Option<String>,
    pub usage: Option<TokenUsage>,
}

/// Compatibility name for the core command-buffer completion input.
pub type CommandCompletionRequest = aicoach_core::CompletionInput;

pub type ChatStream = Pin<Box<dyn Stream<Item = AiResult<String>> + Send + 'static>>;
